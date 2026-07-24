"""
Repository for player data access.
"""

import json
import logging
from datetime import UTC

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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def get_reminder_timestamps_bulk(
        self,
        discord_ids: list[int],
        guild_id: int | None,
    ) -> dict[int, dict[str, int | None]]:
        """Load cooldown timestamps for many reminder subscribers."""
        unique_ids = list(dict.fromkeys(discord_ids))
        timestamps = {
            discord_id: {
                "last_wheel_spin": None,
                "last_trivia_session": None,
            }
            for discord_id in unique_ids
        }
        if not unique_ids:
            return timestamps

        normalized = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            for offset in range(0, len(unique_ids), 900):
                chunk = unique_ids[offset : offset + 900]
                placeholders = ",".join("?" for _ in chunk)
                rows = conn.execute(
                    f"""
                    SELECT discord_id, last_wheel_spin, last_trivia_session
                    FROM players
                    WHERE guild_id = ? AND discord_id IN ({placeholders})
                    """,
                    [normalized, *chunk],
                ).fetchall()
                for row in rows:
                    timestamps[row["discord_id"]] = {
                        "last_wheel_spin": row["last_wheel_spin"],
                        "last_trivia_session": row["last_trivia_session"],
                    }
        return timestamps

    def get_shuffle_inputs(
        self, discord_ids: list[int], guild_id: int | None
    ) -> tuple[list[Player], dict[int, str | None], dict[int, int]]:
        """Load ordered players, last-match dates, and exclusions together."""
        if not discord_ids:
            return [], {}, {}

        guild_id = self.normalize_guild_id(guild_id)
        unique_ids = list(dict.fromkeys(discord_ids))
        placeholders = ",".join("?" * len(unique_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT *
                FROM players
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                (guild_id, *unique_ids),
            )
            rows_by_id = {
                row["discord_id"]: row for row in cursor.fetchall()
            }

        players: list[Player] = []
        last_match_dates: dict[int, str | None] = {}
        exclusion_counts: dict[int, int] = {}
        for discord_id in discord_ids:
            row = rows_by_id.get(discord_id)
            if row is None:
                logger.warning(f"Player not found: discord_id={discord_id}")
                continue
            players.append(self._row_to_player(row))
            last_match_dates[discord_id] = row["last_match_date"]
            exclusion_counts[discord_id] = int(row["exclusion_count"] or 0)

        return players, last_match_dates, exclusion_counts

    def get_by_username(self, username: str, guild_id: int) -> list[dict]:
        """
        Find players whose Discord username matches the provided value (case-insensitive, partial match).

        Args:
            username: Full or partial Discord username (e.g., 'user#1234' or just 'user').
            guild_id: Guild ID to filter by.

        Returns:
            List of dicts containing discord_id and discord_username for each match.
        """
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM players WHERE guild_id = ?", (guild_id,))
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_random_eligible_target(self, guild_id: int, exclude_id: int, min_balance: int = 1) -> "Player | None":
        """Get a random player with positive balance, excluding one player. SQL-level random."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ? AND discord_id != ? AND jopacoin_balance >= ?
                ORDER BY RANDOM() LIMIT 1
                """,
                (guild_id, exclude_id, min_balance),
            )
            row = cursor.fetchone()
            return self._row_to_player(row) if row else None

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
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ?
                ORDER BY
                    COALESCE(jopacoin_balance, 0) DESC,
                    COALESCE(wins, 0) DESC,
                    COALESCE(glicko_rating, 0) DESC,
                    discord_id ASC
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
        guild_id = self.normalize_guild_id(guild_id)
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
                        COALESCE(wins, 0) DESC,
                        discord_id ASC
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
                        COALESCE(wins, 0) DESC,
                        discord_id ASC
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
        guild_id = self.normalize_guild_id(guild_id)
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
                        COALESCE(wins, 0) DESC,
                        discord_id ASC
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
                        COALESCE(wins, 0) DESC,
                        discord_id ASC
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT COUNT(*) as count FROM players WHERE guild_id = ?", (guild_id,))
            row = cursor.fetchone()
            return row["count"] if row else 0

    def exists(self, discord_id: int, guild_id: int) -> bool:
        """Check if a player exists in a guild."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT 1 FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            return cursor.fetchone() is not None

    def update_roles(self, discord_id: int, guild_id: int, roles: list[str]) -> None:
        """Update player's preferred roles."""
        guild_id = self.normalize_guild_id(guild_id)
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

    def update_preferred_region(self, discord_id: int, guild_id: int, region: str | None) -> None:
        """Update the player's explicitly chosen server region ("USE"/"USW")."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET preferred_region = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (region, discord_id, guild_id),
            )

    def update_inferred_region(self, discord_id: int, guild_id: int, region: str | None) -> None:
        """Cache the region inferred from OpenDota play ("USE"/"USW"/"NONE" sentinel)."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET inferred_region = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (region, discord_id, guild_id),
            )

    def update_inferred_regions_bulk(
        self, updates: list[tuple[int, int, str]]
    ) -> None:
        """Cache multiple inferred regions in one transaction."""
        if not updates:
            return
        with self.connection() as conn:
            conn.executemany(
                """
                UPDATE players
                SET inferred_region = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                [
                    (region, discord_id, self.normalize_guild_id(guild_id))
                    for discord_id, guild_id, region in updates
                ],
            )

    def update_glicko_rating(
        self, discord_id: int, guild_id: int, rating: float, rd: float, volatility: float
    ) -> None:
        """Update player's Glicko-2 rating."""
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def get_match_rating_inputs(
        self, discord_ids: list[int], guild_id: int | None
    ) -> dict[int, dict]:
        """Bulk-load every player field used during match rating calculation."""
        unique_ids = list(dict.fromkeys(discord_ids))
        if not unique_ids:
            return {}

        guild_id = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(unique_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT
                    discord_id,
                    current_mmr,
                    glicko_rating,
                    glicko_rd,
                    glicko_volatility,
                    os_mu,
                    os_sigma,
                    last_match_date,
                    created_at,
                    first_calibrated_at
                FROM players
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                unique_ids + [guild_id],
            )
            return {row["discord_id"]: dict(row) for row in cursor.fetchall()}

    def get_last_match_date(self, discord_id: int, guild_id: int) -> tuple | None:
        """
        Get the last_match_date and created_at for a player.

        Returns:
            Tuple (last_match_date, created_at) or None if player not found.
        """
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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


    def get_balance(self, discord_id: int, guild_id: int) -> int:
        """Get a player's jopacoin balance."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return int(row["balance"]) if row else 0

    def get_balances_bulk(
        self, discord_ids: list[int], guild_id: int | None
    ) -> dict[int, int]:
        """Get balances for multiple players on one connection."""
        unique_ids = list(dict.fromkeys(discord_ids))
        if not unique_ids:
            return {}

        guild_id = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(unique_ids))
        balances = dict.fromkeys(unique_ids, 0)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COALESCE(jopacoin_balance, 0) AS balance
                FROM players
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                (guild_id, *unique_ids),
            )
            for row in cursor.fetchall():
                balances[row["discord_id"]] = int(row["balance"])
        return balances

    def advance_dota_streak(
        self, discord_id: int, guild_id: int, today: str, yesterday: str
    ) -> int:
        """Advance the player's Dota daily-play streak for today.

        Returns the streak day-count after the update. If the player already
        had ``today`` recorded, returns the existing streak unchanged (so
        repeat matches the same day pull the same tier without double-counting
        the day toward streak growth). Resets to 1 if the gap is >1 day.

        Returns 0 if the player row doesn't exist.
        """
        return self.advance_dota_streaks_bulk(
            [discord_id], guild_id, today, yesterday
        )[0]

    def advance_dota_streaks_bulk(
        self,
        discord_ids: list[int],
        guild_id: int | None,
        today: str,
        yesterday: str,
    ) -> list[int]:
        """Atomically advance Dota streaks for a player batch.

        The returned list is aligned with ``discord_ids``: input order and
        duplicates are preserved, and missing players produce ``0``. Each
        existing player is updated at most once within the transaction.
        """
        if not discord_ids:
            return []

        guild_id = self.normalize_guild_id(guild_id)
        unique_ids = list(dict.fromkeys(discord_ids))
        placeholders = ",".join("?" for _ in unique_ids)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            rows = cursor.execute(
                f"""
                SELECT discord_id, dota_streak_days, dota_last_played_date
                FROM players
                WHERE guild_id = ? AND discord_id IN ({placeholders})
                """,
                (guild_id, *unique_ids),
            ).fetchall()
            rows_by_id = {int(row["discord_id"]): row for row in rows}

            resulting_streaks: dict[int, int] = {}
            for discord_id in unique_ids:
                row = rows_by_id.get(discord_id)
                if row is None:
                    resulting_streaks[discord_id] = 0
                    continue
                current = int(row["dota_streak_days"] or 0)
                last_date = row["dota_last_played_date"]
                resulting_streaks[discord_id] = (
                    current
                    if last_date == today
                    else current + 1
                    if last_date == yesterday
                    else 1
                )

            # Same-day replays intentionally do not touch updated_at, matching
            # the single-player behavior. BEGIN IMMEDIATE keeps the read and
            # set-based update serialized with concurrent match finalizations.
            cursor.execute(
                f"""
                UPDATE players
                SET dota_streak_days = CASE
                        WHEN dota_last_played_date = ?
                            THEN COALESCE(dota_streak_days, 0) + 1
                        ELSE 1
                    END,
                    dota_last_played_date = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE guild_id = ?
                  AND discord_id IN ({placeholders})
                  AND (dota_last_played_date IS NULL OR dota_last_played_date <> ?)
                """,
                (yesterday, today, guild_id, *unique_ids, today),
            )

        return [resulting_streaks[discord_id] for discord_id in discord_ids]

    def get_dota_streak(self, discord_id: int, guild_id: int) -> tuple[int, str | None]:
        """Read (streak_days, last_played_date) for a player."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            row = cursor.execute(
                "SELECT dota_streak_days, dota_last_played_date FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            ).fetchone()
            if row is None:
                return 0, None
            return int(row["dota_streak_days"] or 0), row["dota_last_played_date"]

    def update_balance(
        self,
        discord_id: int,
        guild_id: int,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> None:
        """Set a player's jopacoin balance to a specific amount."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
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
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

    def add_balance(
        self,
        discord_id: int,
        guild_id: int,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> None:
        """Add or subtract from a player's jopacoin balance."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
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
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

    def try_debit(
        self,
        discord_id: int,
        guild_id: int,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> bool:
        """Atomically debit ``amount`` JC if and only if the player has enough.

        Returns True on success, False if the balance was insufficient. Uses
        a conditional UPDATE so the check and the debit happen in a single
        statement — no TOCTOU window between callers racing on the balance.
        """
        if amount <= 0:
            return True
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
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
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND COALESCE(jopacoin_balance, 0) >= ?
                    """,
                    (amount, discord_id, guild_id, amount),
                )
                debit_rowcount = cursor.rowcount
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)
            if debit_rowcount == 0:
                return False
            cursor.execute(
                """
                UPDATE players
                SET lowest_balance_ever = jopacoin_balance
                WHERE discord_id = ? AND guild_id = ?
                  AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                """,
                (discord_id, guild_id),
            )
            return True

    def add_balance_many(
        self,
        deltas_by_discord_id: dict[int, int],
        guild_id: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> None:
        """
        Apply multiple balance deltas in a single transaction.
        """
        guild_id = self.normalize_guild_id(guild_id)
        if not deltas_by_discord_id:
            return
        with self.connection() as conn:
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
                cursor.executemany(
                    """
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    [
                        (delta, discord_id, guild_id)
                        for discord_id, delta in deltas_by_discord_id.items()
                    ],
                )
                # Track lowest balance for players who had negative deltas
                negative_ids = [
                    did for did, delta in deltas_by_discord_id.items() if delta < 0
                ]
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
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

    def add_balance_batch(
        self,
        balance_updates: list[tuple[int, int, dict | str | None]],
        guild_id: int | None,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
    ) -> None:
        """Apply ordered balance updates with per-update ledger metadata.

        Unlike :meth:`add_balance_many`, this keeps duplicate player entries
        distinct and attaches the matching metadata to each ledger row.
        """
        if not balance_updates:
            return

        guild_id = self.normalize_guild_id(guild_id)
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            for discord_id, amount, metadata in balance_updates:
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
                        UPDATE players
                        SET jopacoin_balance = COALESCE(jopacoin_balance, 0) + ?,
                            updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ? AND guild_id = ?
                        """,
                        (amount, discord_id, guild_id),
                    )
                    if amount < 0:
                        cursor.execute(
                            """
                            UPDATE players
                            SET lowest_balance_ever = jopacoin_balance
                            WHERE discord_id = ? AND guild_id = ?
                              AND (lowest_balance_ever IS NULL
                                   OR jopacoin_balance < lowest_balance_ever)
                            """,
                            (discord_id, guild_id),
                        )
                finally:
                    if has_context:
                        self._clear_economy_ledger_context(cursor)

    def add_balance_with_garnishment(
        self,
        discord_id: int,
        guild_id: int,
        amount: int,
        garnishment_rate: float,
        bankruptcy_penalty_rate: float = 0.0,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> dict[str, int]:
        """
        Add income with garnishment (and optional bankruptcy penalty) in one atomic op.

        When a player has a negative balance (debt), ``garnishment_rate`` fraction
        of the gross is "garnished" toward the debt. The full gross is still
        credited to the balance; the return value reports the split.

        When ``bankruptcy_penalty_rate > 0``, a bankruptcy penalty is debited in
        the same ``BEGIN IMMEDIATE`` txn. The penalty is computed from the
        post-garnishment ``net`` (the portion the player "feels"), using the
        live balance read inside this transaction — so the penalty base cannot
        drift under a concurrent balance flip.

        Args:
            discord_id: Player ID.
            guild_id: Guild ID (normalized to 0 if None).
            amount: Gross income to credit.
            garnishment_rate: Fraction garnished when the player is in debt.
            bankruptcy_penalty_rate: When > 0, the repo computes
                ``penalty = int(net * (1 - bankruptcy_penalty_rate))`` inside
                this atomic txn and debits it, fusing garnishment credit and
                penalty debit. Flooring the penalty (not the kept net) keeps a
                fractional rate from rounding a small net down to zero. Pass
                0.0 (default) to skip the penalty.

        Returns:
            Dict with 'gross', 'garnished', 'net', 'bankruptcy_penalty'.
            - gross: The original income amount
            - garnished: Amount that went toward debt repayment
            - net: Amount the player "feels" (gross - garnished - bankruptcy_penalty)
            - bankruptcy_penalty: Amount debited for the bankruptcy penalty (0 if
              ``bankruptcy_penalty_rate`` was 0 or nothing to penalize)
        """
        guild_id = self.normalize_guild_id(guild_id)
        if amount <= 0:
            # Nothing to credit => nothing to garnish and no "net" to base a
            # penalty on. Callers shouldn't be requesting a penalty here.
            return {"gross": amount, "garnished": 0, "net": amount, "bankruptcy_penalty": 0}

        with self.atomic_transaction() as conn:
            return self._add_balance_with_garnishment_cursor(
                conn.cursor(),
                discord_id,
                guild_id,
                amount,
                garnishment_rate,
                bankruptcy_penalty_rate,
                source=source,
                actor_id=actor_id,
                related_type=related_type,
                related_id=related_id,
                reason=reason,
                metadata=metadata,
            )

    def add_balances_with_garnishment(
        self,
        awards: list[tuple[int, int, float]],
        guild_id: int | None,
        garnishment_rate: float,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
    ) -> list[dict[str, int]]:
        """Credit multiple incomes in one atomic transaction.

        Results align with ``awards``. Each tuple contains
        ``(discord_id, amount, bankruptcy_penalty_rate)``; duplicate players
        are evaluated sequentially against their live balance, matching
        repeated point calls.
        """
        if not awards:
            return []

        guild_id = self.normalize_guild_id(guild_id)
        if all(amount <= 0 for _discord_id, amount, _penalty_rate in awards):
            return [
                {
                    "gross": amount,
                    "garnished": 0,
                    "net": amount,
                    "bankruptcy_penalty": 0,
                }
                for _discord_id, amount, _penalty_rate in awards
            ]

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            return [
                self._add_balance_with_garnishment_cursor(
                    cursor,
                    discord_id,
                    guild_id,
                    amount,
                    garnishment_rate,
                    bankruptcy_penalty_rate,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=reason,
                    metadata=metadata,
                )
                for discord_id, amount, bankruptcy_penalty_rate in awards
            ]

    def _add_balance_with_garnishment_cursor(
        self,
        cursor,
        discord_id: int,
        guild_id: int,
        amount: int,
        garnishment_rate: float,
        bankruptcy_penalty_rate: float,
        *,
        source: str | None,
        actor_id: int | None,
        related_type: str | None,
        related_id: str | int | None,
        reason: str | None,
        metadata: dict | str | None,
    ) -> dict[str, int]:
        """Apply one award using a caller-owned transaction cursor."""
        if amount <= 0:
            return {"gross": amount, "garnished": 0, "net": amount, "bankruptcy_penalty": 0}

        cursor.execute(
            "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
            (discord_id, guild_id),
        )
        row = cursor.fetchone()
        if not row:
            raise ValueError("Player not found.")

        current_balance = int(row["balance"])
        if current_balance >= 0:
            garnished = 0
            net_before_penalty = amount
        else:
            garnished = int(amount * garnishment_rate)
            net_before_penalty = amount - garnished

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
            # Full gross is credited to the balance (garnishment is a
            # bookkeeping split, not a separate debit).
            cursor.execute(
                """
                UPDATE players
                SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (amount, discord_id, guild_id),
            )
        finally:
            if has_context:
                self._clear_economy_ledger_context(cursor)

        if bankruptcy_penalty_rate > 0 and net_before_penalty > 0:
            # Floor the penalty (amount withheld), not the kept net, so a
            # fractional rate never rounds a small net down to zero.
            penalty = int(net_before_penalty * (1 - bankruptcy_penalty_rate))
        else:
            penalty = 0

        if penalty > 0:
            if has_context:
                penalty_reason = (
                    f"{reason} bankruptcy penalty"
                    if reason
                    else "bankruptcy penalty on income"
                )
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=penalty_reason,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (penalty, discord_id, guild_id),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

        return {
            "gross": amount,
            "garnished": garnished,
            "net": net_before_penalty - penalty,
            "bankruptcy_penalty": penalty,
        }

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
        guild_id = self.normalize_guild_id(guild_id)
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

            self._set_economy_ledger_context(
                cursor,
                source="debt_payment",
                actor_id=from_discord_id,
                related_type="player_debt",
                related_id=to_discord_id,
                reason="debt payment sender debit",
                metadata={"amount_paid": actual_amount},
            )
            try:
                # Deduct from sender
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (actual_amount, from_discord_id, guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

            self._set_economy_ledger_context(
                cursor,
                source="debt_payment",
                actor_id=from_discord_id,
                related_type="player_debt",
                related_id=to_discord_id,
                reason="debt payment recipient credit",
                metadata={"amount_paid": actual_amount},
            )
            try:
                # Add to recipient (reduces debt)
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (actual_amount, to_discord_id, guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

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
        tithe: int = 0,
    ) -> dict[str, int]:
        """
        Atomically transfer jopacoin from sender to recipient, crediting the
        fee (and any tithe) to the nonprofit fund in the same transaction.

        Sender pays ``amount + fee + tithe``; recipient receives ``amount``;
        nonprofit fund receives ``fee + tithe``. Folding the nonprofit credit
        into this txn closes a window where the sender debit committed but a
        separate post-commit ``add_to_nonprofit_fund`` call silently failed
        and burned the fee.

        Args:
            from_discord_id: Player sending the tip
            to_discord_id: Player receiving the tip
            guild_id: Guild ID
            amount: Amount to transfer to recipient (must be positive)
            fee: Tip fee credited to nonprofit (>= 0)
            tithe: Extra amount debited from sender and credited to nonprofit
                (e.g. Plains-mana tithe). Defaults to 0.

        Returns:
            Dict with 'amount', 'fee', 'tithe', 'from_new_balance',
            'to_new_balance', 'nonprofit_credit'.

        Raises:
            ValueError if insufficient funds or player not found
        """
        guild_id = self.normalize_guild_id(guild_id)

        if amount <= 0:
            raise ValueError("Amount must be positive.")
        if fee < 0:
            raise ValueError("Fee cannot be negative.")
        if tithe < 0:
            raise ValueError("Tithe cannot be negative.")

        nonprofit_credit = fee + tithe
        total_cost = amount + nonprofit_credit

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

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
                    f"Insufficient balance. You need {total_cost} (tip: {amount}, fee: {fee}"
                    + (f", tithe: {tithe}" if tithe else "")
                    + f"). You have {from_balance}."
                )

            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ? AND guild_id = ?",
                (to_discord_id, guild_id),
            )
            to_row = cursor.fetchone()
            if not to_row:
                raise ValueError("Recipient not found.")

            to_balance = int(to_row["balance"])

            self._set_economy_ledger_context(
                cursor,
                source="tip",
                actor_id=from_discord_id,
                related_type="player_tip",
                related_id=to_discord_id,
                reason="tip sender debit",
                metadata={
                    "amount": amount,
                    "fee": fee,
                    "tithe": tithe,
                    "total_cost": total_cost,
                },
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (total_cost, from_discord_id, guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

            self._set_economy_ledger_context(
                cursor,
                source="tip",
                actor_id=from_discord_id,
                related_type="player_tip",
                related_id=to_discord_id,
                reason="tip recipient credit",
                metadata={"amount": amount, "fee": fee, "tithe": tithe},
            )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, to_discord_id, guild_id),
                )
            finally:
                self._clear_economy_ledger_context(cursor)

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

            if nonprofit_credit > 0:
                self._set_economy_ledger_context(
                    cursor,
                    source="tip",
                    actor_id=from_discord_id,
                    related_type="player_tip",
                    related_id=to_discord_id,
                    reason="tip fee and tithe reserve credit",
                    metadata={
                        "amount": amount,
                        "fee": fee,
                        "tithe": tithe,
                        "nonprofit_credit": nonprofit_credit,
                    },
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
                        (guild_id, nonprofit_credit),
                    )
                finally:
                    self._clear_economy_ledger_context(cursor)

            return {
                "amount": amount,
                "fee": fee,
                "tithe": tithe,
                "nonprofit_credit": nonprofit_credit,
                "from_new_balance": new_from_balance,
                "to_new_balance": to_balance + amount,
            }

    def steal_atomic(
        self,
        thief_discord_id: int,
        victim_discord_id: int,
        guild_id: int,
        amount: int,
        *,
        source: str | None = None,
        actor_id: int | None = None,
        related_type: str | None = None,
        related_id: str | int | None = None,
        reason: str | None = None,
        metadata: dict | str | None = None,
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
        guild_id = self.normalize_guild_id(guild_id)
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

            # Deduct from victim (can go below MAX_DEBT - intentional)
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=f"{reason} victim debit" if reason else None,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance - ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, victim_discord_id, guild_id),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

            # Add to thief
            if has_context:
                self._set_economy_ledger_context(
                    cursor,
                    source=source,
                    actor_id=actor_id,
                    related_type=related_type,
                    related_id=related_id,
                    reason=f"{reason} thief credit" if reason else None,
                    metadata=metadata,
                )
            try:
                cursor.execute(
                    """
                    UPDATE players
                    SET jopacoin_balance = jopacoin_balance + ?, updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (amount, thief_discord_id, guild_id),
                )
            finally:
                if has_context:
                    self._clear_economy_ledger_context(cursor)

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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Get all players with non-negative balance, ordered by balance DESC
            # Skip the top 3 (richest players)
            cursor.execute(
                """
                SELECT discord_id, jopacoin_balance
                FROM players
                WHERE guild_id = ? AND jopacoin_balance >= 0 AND (wins + losses) > 0
                ORDER BY jopacoin_balance DESC
                LIMIT -1 OFFSET 3
                """,
                (guild_id,),
            )
            return [
                {"discord_id": row["discord_id"], "balance": row["jopacoin_balance"]}
                for row in cursor.fetchall()
            ]

    def get_all_registered_players_for_lottery(self, guild_id: int, activity_days: int = 14) -> list[dict]:
        """
        Get recently active players (discord_id only) for lottery selection.

        Only includes players who have played a match within the last
        ``activity_days`` days.

        Returns:
            List of dicts with 'discord_id' for eligible players.
        """
        guild_id = self.normalize_guild_id(guild_id)
        from datetime import datetime, timedelta

        cutoff = (datetime.now(UTC) - timedelta(days=activity_days)).isoformat()
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id FROM players
                WHERE guild_id = ?
                  AND last_match_date IS NOT NULL
                  AND last_match_date >= ?
                """,
                (guild_id, cutoff),
            )
            return [{"discord_id": row["discord_id"]} for row in cursor.fetchall()]

    def get_players_by_games_played(self, guild_id: int) -> list[dict]:
        """
        Get players sorted by total games played (wins + losses).

        Only includes players with at least 1 game played, excluding the top 3
        by balance (same exclusion as stimulus).
        Used for social security distribution.

        Returns:
            List of dicts with 'discord_id' and 'games_played', sorted by games DESC.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, COALESCE(wins, 0) + COALESCE(losses, 0) as games_played
                FROM players
                WHERE guild_id = ?
                  AND COALESCE(wins, 0) + COALESCE(losses, 0) > 0
                  AND discord_id NOT IN (
                      SELECT discord_id FROM players
                      WHERE guild_id = ? AND jopacoin_balance >= 0
                      ORDER BY jopacoin_balance DESC
                      LIMIT 3
                  )
                ORDER BY games_played DESC
                """,
                (guild_id, guild_id),
            )
            return [
                {"discord_id": row["discord_id"], "games_played": row["games_played"]}
                for row in cursor.fetchall()
            ]

    def get_richest_player(self, guild_id: int) -> dict | None:
        """
        Get the single richest player by jopacoin balance.

        Used for 'richest' disbursement method (reverse of neediest).

        Returns:
            Dict with 'discord_id' and 'jopacoin_balance', or None if no players exist.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, jopacoin_balance
                FROM players
                WHERE guild_id = ?
                ORDER BY jopacoin_balance DESC
                LIMIT 1
                """,
                (guild_id,),
            )
            row = cursor.fetchone()
            return {"discord_id": row["discord_id"], "jopacoin_balance": row["jopacoin_balance"]} if row else None

    def get_richest_players(
        self, guild_id: int, limit: int = 5, min_balance: int = 1
    ) -> list[dict]:
        """Return the top ``limit`` players by positive jopacoin balance.

        Used by dig splash events (``richest_n`` pool) so inflation pain can
        land on the concentration of wealth rather than on newer players.
        Excludes zero and negative balances so nobody flagged as a debtor
        picks up an extra debit.

        Returns:
            List of dicts with ``discord_id`` and ``jopacoin_balance``,
            ordered by balance descending.
        """
        guild_id = self.normalize_guild_id(guild_id)
        if limit <= 0:
            return []
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, jopacoin_balance
                FROM players
                WHERE guild_id = ? AND jopacoin_balance >= ?
                ORDER BY jopacoin_balance DESC
                LIMIT ?
                """,
                (guild_id, min_balance, limit),
            )
            return [
                {"discord_id": row["discord_id"], "jopacoin_balance": row["jopacoin_balance"]}
                for row in cursor.fetchall()
            ]


    def increment_wins(self, discord_id: int, guild_id: int) -> None:
        """Increment player's win count."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )



    def apply_match_outcome(self, winning_ids: list[int], losing_ids: list[int], guild_id: int) -> None:
        """
        Apply win/loss increments for a match in a single transaction.
        """
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        """Increment player's exclusion count by 6."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 6,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def increment_exclusion_count_half(self, discord_id: int, guild_id: int) -> None:
        """Increment player's exclusion count by 1.

        Used for conditional players who weren't picked.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
            """,
                (discord_id, guild_id),
            )

    def decay_exclusion_count(self, discord_id: int, guild_id: int) -> None:
        """Decay player's exclusion count by one, stopping at zero."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = MAX(COALESCE(exclusion_count, 0) - 1, 0),
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

        unique_ids = list(dict.fromkeys(discord_ids))
        result: dict[int, list[int]] = {did: [] for did in unique_ids}

        with self.connection() as conn:
            cursor = conn.cursor()
            junction_found = set()
            # Stay below conservative SQLite bind limits for discovery runs
            # that hydrate hundreds of matches at once.
            for offset in range(0, len(unique_ids), 900):
                chunk = unique_ids[offset : offset + 900]
                placeholders = ",".join("?" for _ in chunk)
                cursor.execute(
                    f"""
                    SELECT discord_id, steam_id
                    FROM player_steam_ids
                    WHERE discord_id IN ({placeholders})
                    ORDER BY discord_id, is_primary DESC, added_at ASC
                    """,
                    chunk,
                )

                for row in cursor.fetchall():
                    did = row["discord_id"]
                    sid = row["steam_id"]
                    result[did].append(sid)
                    junction_found.add(did)

            # Fallback to legacy column for players not in junction table
            missing = [did for did in unique_ids if did not in junction_found]
            for offset in range(0, len(missing), 900):
                chunk = missing[offset : offset + 900]
                placeholders = ",".join("?" for _ in chunk)
                cursor.execute(
                    f"SELECT discord_id, steam_id FROM players WHERE discord_id IN ({placeholders})",
                    chunk,
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

    def get_players_needing_region_backfill(self) -> list[dict]:
        """Players whose inferred_region isn't computed yet but who have a Steam ID.

        Returns one entry per ``(discord_id, guild_id)`` with a usable steam_id
        (primary from the junction table, falling back to the legacy
        ``players.steam_id``). Fake users (``discord_id < 0``) are skipped. Used by
        the startup backfill task; only ``NULL`` rows are returned so it converges.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT p.discord_id, p.guild_id,
                       COALESCE(psi.steam_id, p.steam_id) AS steam_id
                FROM players p
                LEFT JOIN player_steam_ids psi
                       ON psi.discord_id = p.discord_id AND psi.is_primary = 1
                WHERE p.inferred_region IS NULL
                  AND p.discord_id > 0
                  AND COALESCE(psi.steam_id, p.steam_id) IS NOT NULL
                """
            )
            return [
                {
                    "discord_id": row["discord_id"],
                    "guild_id": row["guild_id"],
                    "steam_id": row["steam_id"],
                }
                for row in cursor.fetchall()
            ]

    def delete_fake_users(self, guild_id: int) -> int:
        """
        Delete all fake users (discord_id < 0) and their related data in a guild.

        Returns:
            Number of fake users deleted.
        """
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT lowest_balance_ever FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["lowest_balance_ever"] if row and row["lowest_balance_ever"] is not None else None

    def get_lowest_balances_bulk(self, discord_ids: list[int], guild_id: int) -> dict[int, int | None]:
        """Get lowest_balance_ever for multiple players in a single query.

        Returns dict of {discord_id: lowest_balance_ever}.
        """
        if not discord_ids:
            return {}
        normalized_guild = self.normalize_guild_id(guild_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"SELECT discord_id, lowest_balance_ever FROM players WHERE guild_id = ? AND discord_id IN ({placeholders})",
                [normalized_guild] + list(discord_ids),
            )
            return {row["discord_id"]: row["lowest_balance_ever"] for row in cursor.fetchall()}


    # =========================================================================
    # Easter Egg Tracking Methods (JOPA-T expansion)
    # =========================================================================

    def update_personal_best_win_streak(
        self, discord_id: int, guild_id: int, streak: int
    ) -> bool:
        """
        Update personal best win streak if new streak is higher.

        Returns True if the record was updated, False otherwise.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET personal_best_win_streak = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                AND (personal_best_win_streak IS NULL OR personal_best_win_streak < ?)
                """,
                (streak, discord_id, guild_id, streak),
            )
            return cursor.rowcount > 0

    def update_personal_best_win_streaks(
        self, streaks_by_discord_id: dict[int, int], guild_id: int | None
    ) -> dict[int, int]:
        """Atomically compare and update personal-best win streaks in bulk.

        Returns ``{discord_id: previous_best}`` only for records improved by
        this call. Missing players and non-improvements are omitted.
        """
        if not streaks_by_discord_id:
            return {}

        guild_id = self.normalize_guild_id(guild_id)
        discord_ids = list(streaks_by_discord_id)
        placeholders = ",".join("?" * len(discord_ids))
        with self.atomic_transaction() as conn:
            cursor = conn.cursor()
            cursor.execute(
                f"""
                SELECT discord_id, COALESCE(personal_best_win_streak, 0) AS personal_best
                FROM players
                WHERE discord_id IN ({placeholders}) AND guild_id = ?
                """,
                (*discord_ids, guild_id),
            )
            previous_bests = {
                row["discord_id"]: int(row["personal_best"])
                for row in cursor.fetchall()
            }
            improvements = {
                discord_id: streak
                for discord_id, streak in streaks_by_discord_id.items()
                if discord_id in previous_bests
                and streak > previous_bests[discord_id]
            }
            cursor.executemany(
                """
                UPDATE players
                SET personal_best_win_streak = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND COALESCE(personal_best_win_streak, 0) < ?
                """,
                [
                    (streak, discord_id, guild_id, streak)
                    for discord_id, streak in improvements.items()
                ],
            )
            return {
                discord_id: previous_bests[discord_id]
                for discord_id in improvements
            }

    def get_personal_best_win_streak(self, discord_id: int, guild_id: int) -> int:
        """Get player's personal best win streak."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT personal_best_win_streak FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["personal_best_win_streak"] if row and row["personal_best_win_streak"] else 0

    def increment_total_bets_placed(self, discord_id: int, guild_id: int) -> int:
        """
        Increment total_bets_placed by 1 and return the new count.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET total_bets_placed = COALESCE(total_bets_placed, 0) + 1,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            # Return the new count
            cursor.execute(
                "SELECT total_bets_placed FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["total_bets_placed"] if row and row["total_bets_placed"] else 0


    def mark_first_leverage_used(self, discord_id: int, guild_id: int) -> bool:
        """
        Mark that the player has used their first leverage bet.

        Returns True if this was the first time (record updated), False if already marked.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET first_leverage_used = 1,
                    updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                AND (first_leverage_used IS NULL OR first_leverage_used = 0)
                """,
                (discord_id, guild_id),
            )
            return cursor.rowcount > 0


    def get_first_calibrated_at(self, discord_id: int, guild_id: int) -> int | None:
        """
        Get the Unix timestamp when the player first became calibrated.

        Returns:
            Unix timestamp or None if never calibrated
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT first_calibrated_at FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return row["first_calibrated_at"] if row and row["first_calibrated_at"] else None


    def get_registered_player_count(self, guild_id: int) -> int:
        """
        Get total count of registered players in a guild.

        Used for quorum calculation in disbursement voting.
        """
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def try_purchase_pingedash(
        self,
        discord_id: int,
        guild_id: int,
        *,
        cost: int,
        now: int,
        cooldown_seconds: int,
    ) -> dict[str, int | str | bool | None]:
        """Atomically charge for /shop pingedash and claim its persistent cooldown."""
        return self._try_purchase_paid_ping(
            discord_id,
            guild_id,
            cost=cost,
            now=now,
            cooldown_seconds=cooldown_seconds,
            command_name="pingedash",
        )

    def try_purchase_pingedkevin(
        self,
        discord_id: int,
        guild_id: int,
        *,
        cost: int,
        now: int,
        cooldown_seconds: int,
    ) -> dict[str, int | str | bool | None]:
        """Atomically charge for /shop pingedkevin and claim its cooldown."""
        return self._try_purchase_paid_ping(
            discord_id,
            guild_id,
            cost=cost,
            now=now,
            cooldown_seconds=cooldown_seconds,
            command_name="pingedkevin",
        )

    def _try_purchase_paid_ping(
        self,
        discord_id: int,
        guild_id: int,
        *,
        cost: int,
        now: int,
        cooldown_seconds: int,
        command_name: str,
    ) -> dict[str, int | str | bool | None]:
        """Atomically charge and claim an independent paid-ping cooldown."""
        command_config = {
            "pingedash": ("last_pingedash", "Pingedash"),
            "pingedkevin": ("last_pingedkevin", "PingedKevin"),
        }
        try:
            cooldown_column, display_name = command_config[command_name]
        except KeyError as exc:
            raise ValueError(f"Unsupported paid ping command: {command_name}") from exc

        if cost < 0:
            raise ValueError(f"{display_name} cost cannot be negative")
        if cooldown_seconds < 0:
            raise ValueError(f"{display_name} cooldown cannot be negative")

        guild_id = self.normalize_guild_id(guild_id)
        cooldown_cutoff = now - cooldown_seconds
        with self.connection() as conn:
            cursor = conn.cursor()
            self._set_economy_ledger_context(
                cursor,
                source=command_name,
                actor_id=discord_id,
                related_type="command",
                related_id=command_name,
                reason=f"{command_name} purchase",
            )
            try:
                cursor.execute(
                    f"""
                    UPDATE players
                    SET jopacoin_balance = COALESCE(jopacoin_balance, 0) - ?,
                        {cooldown_column} = ?,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE discord_id = ? AND guild_id = ?
                      AND COALESCE(jopacoin_balance, 0) >= ?
                      AND ({cooldown_column} IS NULL OR {cooldown_column} <= ?)
                    """,
                    (cost, now, discord_id, guild_id, cost, cooldown_cutoff),
                )
                purchased = cursor.rowcount > 0
            finally:
                self._clear_economy_ledger_context(cursor)

            if purchased:
                cursor.execute(
                    """
                    UPDATE players
                    SET lowest_balance_ever = jopacoin_balance
                    WHERE discord_id = ? AND guild_id = ?
                      AND (lowest_balance_ever IS NULL OR jopacoin_balance < lowest_balance_ever)
                    """,
                    (discord_id, guild_id),
                )
                balance = cursor.execute(
                    """
                    SELECT jopacoin_balance
                    FROM players
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (discord_id, guild_id),
                ).fetchone()["jopacoin_balance"]
                return {
                    "success": True,
                    "reason": None,
                    "balance": int(balance or 0),
                    "cooldown_ends_at": now + cooldown_seconds,
                }

            row = cursor.execute(
                f"""
                SELECT jopacoin_balance, {cooldown_column} AS last_paid_ping
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            ).fetchone()
            if row is None:
                return {
                    "success": False,
                    "reason": "not_registered",
                    "balance": None,
                    "cooldown_ends_at": None,
                }

            balance = int(row["jopacoin_balance"] or 0)
            last_paid_ping = row["last_paid_ping"]
            if last_paid_ping is not None and int(last_paid_ping) > cooldown_cutoff:
                return {
                    "success": False,
                    "reason": "on_cooldown",
                    "balance": balance,
                    "cooldown_ends_at": int(last_paid_ping) + cooldown_seconds,
                }
            return {
                "success": False,
                "reason": "insufficient_balance",
                "balance": balance,
                "cooldown_ends_at": None,
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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def get_wheel_pardon(self, discord_id: int, guild_id: int) -> bool:
        """Get whether a player has an active COMEBACK wheel pardon token."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT has_wheel_pardon FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            return bool(row and row["has_wheel_pardon"])

    def set_wheel_pardon(self, discord_id: int, guild_id: int, value: int) -> None:
        """Set a player's COMEBACK wheel pardon token (1=active, 0=inactive)."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET has_wheel_pardon = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                """,
                (value, discord_id, guild_id),
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
        guild_id = self.normalize_guild_id(guild_id)
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
        self, discord_id: int, guild_id: int | None, result: int, spin_time: int,
        is_bankrupt: bool = False, is_golden: bool = False,
        *,
        outcome_code: str | None = None,
        is_bonus: bool = False,
        event_id: str | None = None,
        outcome_metadata: dict | None = None,
    ) -> int:
        """
        Log a wheel spin result for gambling history tracking.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID (None for DMs)
            result: Spin result (positive for win, negative for bankrupt, 0 for lose turn)
            spin_time: Unix timestamp of the spin
            is_bankrupt: True if this spin was on the bankruptcy wheel
            is_golden: True if this spin was on the golden wheel
            outcome_code: Canonical resolved wedge code for exact history
            is_bonus: True if this was a bonus spin that did not consume cooldown
            event_id: Stable interaction/event identifier for related effects
            outcome_metadata: JSON-safe resolved outcome details

        Returns:
            The spin_id of the created record
        """
        guild_id = self.normalize_guild_id(guild_id)
        metadata_json = (
            json.dumps(outcome_metadata, sort_keys=True, separators=(",", ":"))
            if outcome_metadata is not None
            else None
        )
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO wheel_spins (
                    guild_id, discord_id, result, spin_time, is_bankrupt,
                    is_golden, outcome_code, is_bonus, event_id, outcome_metadata
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    guild_id,
                    discord_id,
                    result,
                    spin_time,
                    1 if is_bankrupt else 0,
                    1 if is_golden else 0,
                    outcome_code,
                    1 if is_bonus else 0,
                    event_id,
                    metadata_json,
                ),
            )
            return cursor.lastrowid

    def get_last_normal_wheel_spin(self, guild_id: int | None) -> dict | None:
        """
        Get the most recent normal-wheel (non-bankrupt) spin in this guild.

        Used by CHAIN_REACTION bankrupt wheel mechanic.

        Args:
            guild_id: Guild ID to filter by

        Returns:
            Dict with 'result' (int) and 'discord_id' (int), or None if no spin found
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT result, discord_id
                FROM wheel_spins
                WHERE guild_id = ? AND (is_bankrupt = 0 OR is_bankrupt IS NULL)
                ORDER BY spin_time DESC
                LIMIT 1
                """,
                (guild_id,),
            )
            row = cursor.fetchone()
            if row:
                return {"result": row["result"], "discord_id": row["discord_id"]}
            return None

    def get_wheel_spin_history(self, discord_id: int, guild_id: int | None = None) -> list[dict]:
        """
        Get wheel spin history for a player.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID (optional, filters to specific guild if provided)

        Returns:
            List of dicts with 'result' and 'spin_time' keys, sorted by spin_time
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT result, spin_time
                FROM wheel_spins
                WHERE discord_id = ? AND guild_id = ?
                ORDER BY spin_time ASC
                """,
                (discord_id, guild_id),
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

    def get_steam_id_owner(self, steam_id: int) -> int | None:
        """
        Return the discord_id that globally owns ``steam_id``, or None.

        Mirrors the conflict check in :meth:`add_steam_id`: the junction
        table is authoritative (its ``UNIQUE(steam_id)`` is the global
        guarantee), with a fallback to the legacy ``players.steam_id`` column
        for rows not yet migrated. Steam accounts are globally unique (not
        per-guild), so this lookup is intentionally cross-guild.
        """
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT discord_id FROM player_steam_ids WHERE steam_id = ?",
                (steam_id,),
            )
            row = cursor.fetchone()
            if row:
                return row["discord_id"]

            # Fallback to legacy column for players not in the junction table.
            cursor.execute(
                "SELECT discord_id FROM players WHERE steam_id = ?",
                (steam_id,),
            )
            row = cursor.fetchone()
            return row["discord_id"] if row else None

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

    def add_steam_ids_bulk(
        self, steam_ids: list[tuple[int, int]]
    ) -> list[dict[str, bool | str]]:
        """Backfill Steam IDs in order using one atomic transaction.

        Results align with ``steam_ids`` and report conflicts without aborting
        unrelated successes. The first successful ID for a player is primary,
        matching repeated ``get_steam_ids`` + ``add_steam_id`` calls.
        """
        if not steam_ids:
            return []

        import time

        discord_ids = list(dict.fromkeys(discord_id for discord_id, _ in steam_ids))
        candidate_ids = list(dict.fromkeys(steam_id for _, steam_id in steam_ids))
        discord_placeholders = ",".join("?" for _ in discord_ids)
        steam_placeholders = ",".join("?" for _ in candidate_ids)

        with self.atomic_transaction() as conn:
            cursor = conn.cursor()

            cursor.execute(
                f"""
                SELECT discord_id
                FROM player_steam_ids
                WHERE discord_id IN ({discord_placeholders})
                """,
                discord_ids,
            )
            players_with_ids = {int(row["discord_id"]) for row in cursor.fetchall()}
            cursor.execute(
                f"""
                SELECT discord_id
                FROM players
                WHERE discord_id IN ({discord_placeholders})
                  AND steam_id IS NOT NULL
                """,
                discord_ids,
            )
            players_with_ids.update(int(row["discord_id"]) for row in cursor.fetchall())

            cursor.execute(
                f"""
                SELECT steam_id, discord_id
                FROM player_steam_ids
                WHERE steam_id IN ({steam_placeholders})
                """,
                candidate_ids,
            )
            junction_owners = {
                int(row["steam_id"]): int(row["discord_id"])
                for row in cursor.fetchall()
            }
            cursor.execute(
                f"""
                SELECT steam_id, discord_id
                FROM players
                WHERE steam_id IN ({steam_placeholders})
                """,
                candidate_ids,
            )
            legacy_owners: dict[int, set[int]] = {}
            for row in cursor.fetchall():
                legacy_owners.setdefault(int(row["steam_id"]), set()).add(
                    int(row["discord_id"])
                )

            results: list[dict[str, bool | str]] = []
            for discord_id, steam_id in steam_ids:
                junction_owner = junction_owners.get(steam_id)
                legacy_conflict = any(
                    owner != discord_id
                    for owner in legacy_owners.get(steam_id, set())
                )
                if (
                    junction_owner is not None
                    and junction_owner != discord_id
                ) or legacy_conflict:
                    results.append({
                        "success": False,
                        "is_primary": False,
                        "error": (
                            f"Steam ID {steam_id} is already linked to another player"
                        ),
                    })
                    continue

                is_primary = discord_id not in players_with_ids
                if is_primary:
                    cursor.execute(
                        "UPDATE player_steam_ids SET is_primary = 0 WHERE discord_id = ?",
                        (discord_id,),
                    )
                cursor.execute(
                    """
                    INSERT INTO player_steam_ids
                        (discord_id, steam_id, is_primary, added_at)
                    VALUES (?, ?, ?, ?)
                    ON CONFLICT(discord_id, steam_id)
                    DO UPDATE SET is_primary = excluded.is_primary
                    """,
                    (
                        discord_id,
                        steam_id,
                        1 if is_primary else 0,
                        int(time.time()),
                    ),
                )
                if is_primary:
                    cursor.execute(
                        """
                        UPDATE players
                        SET steam_id = ?, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ?
                        """,
                        (steam_id, discord_id),
                    )

                players_with_ids.add(discord_id)
                junction_owners[steam_id] = discord_id
                if is_primary:
                    legacy_owners.setdefault(steam_id, set()).add(discord_id)
                results.append({
                    "success": True,
                    "is_primary": is_primary,
                })

        return results

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
                # Not in the junction table. Handle a legacy-only Steam ID that
                # lives solely in players.steam_id (never migrated/relinked):
                # clear it so /player unlink succeeds instead of reporting a
                # spurious failure.
                cursor.execute(
                    "UPDATE players SET steam_id = NULL, updated_at = CURRENT_TIMESTAMP "
                    "WHERE discord_id = ? AND steam_id = ?",
                    (discord_id, steam_id),
                )
                return cursor.rowcount > 0

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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def try_claim_double_or_nothing(
        self, discord_id: int, guild_id: int, now: int, cooldown_seconds: int
    ) -> bool:
        """
        Atomically check cooldown and claim a Double or Nothing spin.

        This prevents race conditions where concurrent requests could both pass
        the cooldown check before either sets the new timestamp.

        Args:
            discord_id: Player's Discord ID
            guild_id: Guild ID
            now: Current Unix timestamp
            cooldown_seconds: Required cooldown between spins

        Returns:
            True if the spin was claimed (cooldown passed), False if still on cooldown
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            # Atomic check-and-set: only update if cooldown has passed
            cursor.execute(
                """
                UPDATE players
                SET last_double_or_nothing = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND (last_double_or_nothing IS NULL OR last_double_or_nothing < ?)
                """,
                (now, discord_id, guild_id, now - cooldown_seconds),
            )
            # If rowcount > 0, the update happened (cooldown passed)
            return cursor.rowcount > 0

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
        guild_id = self.normalize_guild_id(guild_id)
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
        guild_id = self.normalize_guild_id(guild_id)
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

    def get_player_above(
        self,
        discord_id: int,
        guild_id: int,
        min_balance: int | None = None,
    ) -> Player | None:
        """
        Get the nearest eligible player above on the balance leaderboard.

        Used for Red Shell wheel mechanic - steals from the player ahead of you.

        Args:
            discord_id: The player's Discord ID
            guild_id: Guild ID
            min_balance: Optional minimum balance for the target

        Returns:
            Player above the user meeting ``min_balance``, or None if no one is eligible
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            # Read the complete canonical balance-leaderboard sort key.
            cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance,
                       COALESCE(wins, 0) AS wins,
                       COALESCE(glicko_rating, 0) AS rating
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            user_row = cursor.fetchone()
            if not user_row:
                return None

            user_balance = int(user_row["balance"])
            user_wins = int(user_row["wins"])
            user_rating = float(user_row["rating"])
            eligible_min_balance = user_balance if min_balance is None else min_balance

            # Reverse the canonical ordering to select the closest qualifying row
            # ahead of the user: balance, wins, Glicko, then Discord ID.
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ?
                  AND COALESCE(jopacoin_balance, 0) >= ?
                  AND (
                    COALESCE(jopacoin_balance, 0) > ?
                    OR (
                      COALESCE(jopacoin_balance, 0) = ?
                      AND (
                        COALESCE(wins, 0) > ?
                        OR (
                          COALESCE(wins, 0) = ?
                          AND (
                            COALESCE(glicko_rating, 0) > ?
                            OR (
                              COALESCE(glicko_rating, 0) = ?
                              AND discord_id < ?
                            )
                          )
                        )
                      )
                    )
                )
                ORDER BY COALESCE(jopacoin_balance, 0) ASC,
                         COALESCE(wins, 0) ASC,
                         COALESCE(glicko_rating, 0) ASC,
                         discord_id DESC
                LIMIT 1
                """,
                (
                    guild_id,
                    eligible_min_balance,
                    user_balance,
                    user_balance,
                    user_wins,
                    user_wins,
                    user_rating,
                    user_rating,
                    discord_id,
                ),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_player_below(self, discord_id: int, guild_id: int) -> Player | None:
        """
        Get the player ranked one position lower on the balance leaderboard.

        Used for Banana Peel wheel mechanic - the player behind slips on the peel.
        Mirror of get_player_above with the comparison inverted.

        Args:
            discord_id: The player's Discord ID
            guild_id: Guild ID

        Returns:
            Player object of the player ranked below, or None if user is last or not found
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT COALESCE(jopacoin_balance, 0) AS balance,
                       COALESCE(wins, 0) AS wins,
                       COALESCE(glicko_rating, 0) AS rating
                FROM players
                WHERE discord_id = ? AND guild_id = ?
                """,
                (discord_id, guild_id),
            )
            user_row = cursor.fetchone()
            if not user_row:
                return None

            user_balance = int(user_row["balance"])
            user_wins = int(user_row["wins"])
            user_rating = float(user_row["rating"])

            # Use the canonical balance-leaderboard ordering to find the closest
            # row behind the user.
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ? AND (
                    COALESCE(jopacoin_balance, 0) < ?
                    OR (
                      COALESCE(jopacoin_balance, 0) = ?
                      AND (
                        COALESCE(wins, 0) < ?
                        OR (
                          COALESCE(wins, 0) = ?
                          AND (
                            COALESCE(glicko_rating, 0) < ?
                            OR (
                              COALESCE(glicko_rating, 0) = ?
                              AND discord_id > ?
                            )
                          )
                        )
                      )
                    )
                )
                ORDER BY COALESCE(jopacoin_balance, 0) DESC,
                         COALESCE(wins, 0) DESC,
                         COALESCE(glicko_rating, 0) DESC,
                         discord_id ASC
                LIMIT 1
                """,
                (
                    guild_id,
                    user_balance,
                    user_balance,
                    user_wins,
                    user_wins,
                    user_rating,
                    user_rating,
                    discord_id,
                ),
            )
            row = cursor.fetchone()

            if not row:
                return None

            return self._row_to_player(row)

    def get_leaderboard_bottom(
        self, guild_id: int, limit: int = 3, min_balance: int = 1
    ) -> list[Player]:
        """
        Get players with the lowest positive balance, ordered ascending.

        Used by HEIST golden wheel mechanic (steal from bottom 3 positive-balance players).

        Args:
            guild_id: Guild ID
            limit: Maximum number of players to return
            min_balance: Minimum balance threshold (exclusive of debt players)

        Returns:
            List of Player objects sorted by balance ascending
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT * FROM players
                WHERE guild_id = ? AND COALESCE(jopacoin_balance, 0) >= ?
                ORDER BY COALESCE(jopacoin_balance, 0) ASC,
                         COALESCE(wins, 0) ASC,
                         COALESCE(glicko_rating, 0) ASC,
                         discord_id DESC
                LIMIT ?
                """,
                (guild_id, min_balance, limit),
            )
            rows = cursor.fetchall()
            return [self._row_to_player(row) for row in rows]

    def get_total_positive_balance(self, guild_id: int) -> int:
        """
        Get sum of all positive jopacoin balances in the guild.

        Used by DIVIDEND golden wheel mechanic (reward proportional to server wealth).

        Args:
            guild_id: Guild ID

        Returns:
            Total positive balance across all guild members (0 if none)
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT COALESCE(SUM(jopacoin_balance), 0) AS total
                FROM players
                WHERE guild_id = ? AND COALESCE(jopacoin_balance, 0) > 0
                """,
                (guild_id,),
            )
            row = cursor.fetchone()
            return int(row["total"]) if row else 0

    def _row_to_player(self, row) -> Player:
        """Convert database row to Player object."""
        preferred_roles = json.loads(row["preferred_roles"]) if row["preferred_roles"] else None

        # Handle os_mu and os_sigma which may not exist in older schemas
        keys = row.keys()
        os_mu = row["os_mu"] if "os_mu" in keys else None
        os_sigma = row["os_sigma"] if "os_sigma" in keys else None
        steam_id = row["steam_id"] if "steam_id" in keys else None
        guild_id = row["guild_id"] if "guild_id" in keys else None

        # Easter egg tracking fields (may not exist in older schemas)
        personal_best_win_streak = row["personal_best_win_streak"] if "personal_best_win_streak" in keys else 0
        total_bets_placed = row["total_bets_placed"] if "total_bets_placed" in keys else 0
        first_leverage_used = bool(row["first_leverage_used"]) if "first_leverage_used" in keys else False

        # Solo grinder detection fields (may not exist in older schemas)
        is_solo_grinder = bool(row["is_solo_grinder"]) if "is_solo_grinder" in keys else False
        solo_grinder_checked_at = row["solo_grinder_checked_at"] if "solo_grinder_checked_at" in keys else None

        # Server-region preference fields (may not exist in older schemas)
        preferred_region = row["preferred_region"] if "preferred_region" in keys else None
        inferred_region = row["inferred_region"] if "inferred_region" in keys else None

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
            personal_best_win_streak=personal_best_win_streak or 0,
            total_bets_placed=total_bets_placed or 0,
            first_leverage_used=first_leverage_used,
            is_solo_grinder=is_solo_grinder,
            solo_grinder_checked_at=solo_grinder_checked_at,
            preferred_region=preferred_region,
            inferred_region=inferred_region,
        )

    # --- Trivia cooldown ---

    def get_last_trivia_session(self, discord_id: int, guild_id: int) -> int | None:
        """Get the timestamp of a player's last trivia session."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT last_trivia_session FROM players WHERE discord_id = ? AND guild_id = ?",
                (discord_id, guild_id),
            )
            row = cursor.fetchone()
            if not row or row["last_trivia_session"] is None:
                return None
            return int(row["last_trivia_session"])

    def try_claim_trivia_session(self, discord_id: int, guild_id: int, now: int, cooldown_seconds: int) -> bool:
        """Atomically check cooldown and claim a trivia session."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                UPDATE players
                SET last_trivia_session = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ? AND guild_id = ?
                  AND (last_trivia_session IS NULL OR last_trivia_session < ?)
                """,
                (now, discord_id, guild_id, now - cooldown_seconds),
            )
            return cursor.rowcount > 0

    def reset_trivia_cooldown(self, discord_id: int, guild_id: int) -> bool:
        """Reset a player's trivia cooldown by clearing last_trivia_session.

        Returns True only when a cooldown was actually cleared. The
        ``last_trivia_session IS NOT NULL`` guard stops rowcount from counting a
        matched-but-unchanged player row, so a no-op reset reports False.
        """
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "UPDATE players SET last_trivia_session = NULL "
                "WHERE discord_id = ? AND guild_id = ? AND last_trivia_session IS NOT NULL",
                (discord_id, guild_id),
            )
            return cursor.rowcount > 0

    def record_trivia_session(
        self, discord_id: int, guild_id: int, streak: int, jc_earned: int, played_at: int
    ) -> None:
        """Record a completed trivia session for leaderboard tracking."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO trivia_sessions (discord_id, guild_id, streak, jc_earned, played_at)
                VALUES (?, ?, ?, ?, ?)
                """,
                (discord_id, guild_id, streak, jc_earned, played_at),
            )

    def get_trivia_leaderboard(
        self, guild_id: int, since_timestamp: int, limit: int = 3
    ) -> list[dict]:
        """Get top trivia players by best streak in time window."""
        guild_id = self.normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                SELECT discord_id, MAX(streak) as best_streak
                FROM trivia_sessions
                WHERE guild_id = ? AND played_at >= ?
                GROUP BY discord_id
                ORDER BY best_streak DESC, discord_id ASC
                LIMIT ?
                """,
                (guild_id, since_timestamp, limit),
            )
            return [
                {"discord_id": row["discord_id"], "best_streak": row["best_streak"]}
                for row in cursor.fetchall()
            ]
