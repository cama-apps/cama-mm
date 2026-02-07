"""
Database management for player data, matches, and ratings.
Uses SQLite for lightweight, file-based storage.
"""

import json
import logging
import os
import sqlite3
import uuid
from contextlib import contextmanager
from typing import TYPE_CHECKING, Optional

from config import NEW_PLAYER_EXCLUSION_BOOST
from infrastructure.schema_manager import SchemaManager
from utils.debug_logging import debug_log as _database_debug_log

if TYPE_CHECKING:
    from domain.models.player import Player

logger = logging.getLogger("cama_bot.database")

# Default database path - can be overridden via environment variable
DEFAULT_DB_PATH = "cama_shuffle.db"


class Database:
    """Manages SQLite database for the matchmaking system."""

    def __init__(self, db_path: str | None = None):
        """
        Initialize database connection.

        Args:
            db_path: Path to SQLite database file. If not provided,
                     uses DB_PATH environment variable or defaults to cama_shuffle.db
        """
        # region agent log
        _database_debug_log(
            "H2",
            "database.py:Database.__init__",
            "entering Database.__init__",
            {"db_path": db_path},
            run_id="pre-fix",
        )
        # endregion agent log

        raw_path = db_path or os.getenv("DB_PATH", DEFAULT_DB_PATH)
        self._is_memory = raw_path == ":memory:"
        self._memory_connection: sqlite3.Connection | None = None
        self._use_uri = False

        if self._is_memory:
            unique_name = uuid.uuid4().hex
            self.db_path = f"file:memdb_{unique_name}?mode=memory&cache=shared"
            self._use_uri = True
            # For in-memory databases with cache=shared, we must keep at least one
            # connection open at all times or the database is destroyed.
            # Open it now BEFORE schema initialization.
            self._memory_connection = sqlite3.connect(self.db_path, uri=True)
            self._memory_connection.row_factory = sqlite3.Row
        else:
            self.db_path = raw_path
        logger.info(f"Using database path: {self.db_path}")

        # Initialize schema via SchemaManager
        self.schema_manager = SchemaManager(self.db_path, use_uri=self._use_uri)
        self.init_database()

        # region agent log
        _database_debug_log(
            "H2",
            "database.py:Database.__init__",
            "schema initialization completed",
            {"db_path": self.db_path},
            run_id="pre-fix",
        )
        # endregion agent log

    def get_connection(self) -> sqlite3.Connection:
        """Get database connection."""
        if self._is_memory:
            if self._memory_connection is None:
                conn = sqlite3.connect(self.db_path, uri=self._use_uri)
                conn.row_factory = sqlite3.Row
                self._memory_connection = conn
            return self._memory_connection

        conn = sqlite3.connect(self.db_path, uri=self._use_uri)
        conn.row_factory = sqlite3.Row  # Enable column access by name
        if not self._is_memory:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA busy_timeout=5000")
        return conn

    @contextmanager
    def connection(self):
        """
        Context manager for database connections.

        Automatically commits on success, rolls back on exception,
        and always closes the connection.

        Usage:
            with db.connection() as conn:
                cursor = conn.cursor()
                cursor.execute(...)
                # Auto-commits if no exception, rolls back otherwise
        """
        conn = self.get_connection()
        should_close = not self._is_memory
        try:
            yield conn
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        finally:
            if should_close:
                conn.close()

    def init_database(self):
        """Initialize database schema via SchemaManager (idempotent)."""
        self.schema_manager.initialize()

    def add_player(
        self,
        discord_id: int,
        discord_username: str,
        dotabuff_url: str | None = None,
        initial_mmr: int | None = None,
        preferred_roles: list[str] | None = None,
        main_role: str | None = None,
        glicko_rating: float | None = None,
        glicko_rd: float | None = None,
        glicko_volatility: float | None = None,
    ):
        """
        Add a new player to the database.

        Raises ValueError if player already exists (prevents accidental overwrites).
        Use update_player_username() or other update methods to modify existing players.

        Args:
            discord_id: Discord user ID
            discord_username: Discord username
            dotabuff_url: Optional Dotabuff profile URL
            initial_mmr: Initial MMR seed
            preferred_roles: List of preferred roles
            main_role: Primary role from Dotabuff
            glicko_rating: Initial Glicko-2 rating (if None, will be seeded from MMR)
            glicko_rd: Initial Glicko-2 rating deviation
            glicko_volatility: Initial Glicko-2 volatility

        Raises:
            ValueError: If player with this discord_id already exists
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if player already exists
            cursor.execute("SELECT discord_id FROM players WHERE discord_id = ?", (discord_id,))
            if cursor.fetchone():
                raise ValueError(
                    f"Player with Discord ID {discord_id} already exists. Cannot overwrite existing player data."
                )

            roles_json = json.dumps(preferred_roles) if preferred_roles else None

            cursor.execute(
                """
                INSERT INTO players
                (discord_id, discord_username, dotabuff_url, initial_mmr, current_mmr,
                 preferred_roles, main_role, glicko_rating, glicko_rd, glicko_volatility,
                 exclusion_count, jopacoin_balance, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 3, CURRENT_TIMESTAMP)
            """,
                (
                    discord_id,
                    discord_username,
                    dotabuff_url,
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

    def update_player_glicko_rating(
        self, discord_id: int, rating: float, rd: float, volatility: float
    ):
        """
        Update player's Glicko-2 rating after a match.

        Args:
            discord_id: Discord user ID
            rating: New Glicko-2 rating
            rd: New rating deviation
            volatility: New volatility
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                UPDATE players
                SET glicko_rating = ?, glicko_rd = ?, glicko_volatility = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (rating, rd, volatility, discord_id),
            )

    def get_player_glicko_rating(self, discord_id: int) -> tuple[float, float, float] | None:
        """
        Get player's Glicko-2 rating data.

        Args:
            discord_id: Discord user ID

        Returns:
            Tuple of (rating, rd, volatility) or None if not found
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                SELECT glicko_rating, glicko_rd, glicko_volatility
                FROM players
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

            row = cursor.fetchone()

            if row and row[0] is not None:
                return (row[0], row[1], row[2])
            return None

    def get_player(self, discord_id: int) -> Optional["Player"]:
        """
        Get player data by Discord ID.

        Args:
            discord_id: Discord user ID

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

    def get_player_balance(self, discord_id: int) -> int:
        """Return the current jopacoin balance for a player."""
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                "SELECT COALESCE(jopacoin_balance, 0) as balance FROM players WHERE discord_id = ?",
                (discord_id,),
            )
            row = cursor.fetchone()
            return int(row["balance"]) if row else 0

    def get_all_players(self) -> list["Player"]:
        """Get all players from database."""
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute("SELECT * FROM players")
            rows = cursor.fetchall()

            return [self._row_to_player(row) for row in rows]

    def get_players_by_ids(self, discord_ids: list[int]) -> list["Player"]:
        """
        Get multiple players by their Discord IDs.

        IMPORTANT: Returns players in the SAME ORDER as the input discord_ids.
        This is critical for maintaining the mapping between Discord IDs and Player objects.

        Args:
            discord_ids: List of Discord user IDs

        Returns:
            List of Player objects in the same order as input discord_ids
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

            id_to_row = {}
            for row in rows:
                discord_id = row["discord_id"]
                if discord_id in id_to_row:
                    logger.warning(
                        f"Duplicate player entry found in database: discord_id={discord_id}, name={row['discord_username']}"
                    )
                    continue
                id_to_row[discord_id] = row

            players = []
            for discord_id in discord_ids:
                if discord_id not in id_to_row:
                    logger.warning(f"Player with discord_id={discord_id} not found in database")
                    continue

                row = id_to_row[discord_id]
                players.append(self._row_to_player(row))

            return players

    def _row_to_player(self, row: sqlite3.Row) -> "Player":
        from domain.models.player import Player

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
        )

    def record_match(
        self,
        radiant_team_ids: list[int] = None,
        dire_team_ids: list[int] = None,
        winning_team: str = None,  # "radiant" or "dire"
        # Backward compatibility: support old team1/team2 format
        team1_ids: list[int] = None,
        team2_ids: list[int] = None,
        dotabuff_match_id: str | None = None,
        notes: str | None = None,
    ) -> int:
        """
        Record a match result.

        New API (Radiant/Dire):
            - team1_players in DB = radiant_team_ids
            - team2_players in DB = dire_team_ids
            - winning_team in DB = 1 (Radiant won) or 2 (Dire won)
            - match_participants.side = 'radiant' or 'dire'

        Old API (team1/team2 with int winning_team):
            - Kept for backward compatibility; treats team1 as Radiant, team2 as Dire.

        Args:
            radiant_team_ids: Discord IDs of Radiant team players (new API)
            dire_team_ids: Discord IDs of Dire team players (new API)
            winning_team: "radiant" or "dire" (new API) or 1/2 (old API)
            team1_ids: Discord IDs of team 1 players (old API - backward compat)
            team2_ids: Discord IDs of team 2 players (old API - backward compat)
            dotabuff_match_id: Optional Dotabuff match ID
            notes: Optional notes about the match

        Returns:
            Match ID
        """
        # Determine which API is being used and normalize to team1=Radiant, team2=Dire
        isinstance(winning_team, str)

        if isinstance(winning_team, int):
            # Old API: team1/team2 with winning_team as 1 or 2
            if team1_ids is not None and team2_ids is not None:
                pass  # Use explicit keyword args
            elif radiant_team_ids is not None and dire_team_ids is not None:
                # Positional args used for old API
                team1_ids = radiant_team_ids
                team2_ids = dire_team_ids
            else:
                raise ValueError("Old API requires team1_ids and team2_ids")
            winning_team_db = winning_team
            # Old API: treat team1 as Radiant, team2 as Dire going forward
            set(team1_ids)
            set(team2_ids)
        elif isinstance(winning_team, str):
            # New API: Radiant/Dire directly
            if winning_team not in ("radiant", "dire"):
                raise ValueError(f"winning_team must be 'radiant' or 'dire', got '{winning_team}'")
            if radiant_team_ids is None or dire_team_ids is None:
                raise ValueError("New API requires radiant_team_ids and dire_team_ids")
            # Store team1 = Radiant, team2 = Dire (no swapping)
            team1_ids = radiant_team_ids
            team2_ids = dire_team_ids
            winning_team_db = 1 if winning_team == "radiant" else 2
            set(radiant_team_ids)
            set(dire_team_ids)
        else:
            raise ValueError("winning_team must be 'radiant'/'dire' (str) or 1/2 (int)")

        with self.connection() as conn:
            cursor = conn.cursor()

            # Insert match record (team1=Radiant, team2=Dire)
            cursor.execute(
                """
                INSERT INTO matches (team1_players, team2_players, winning_team, dotabuff_match_id, notes)
                VALUES (?, ?, ?, ?, ?)
            """,
                (
                    json.dumps(team1_ids),
                    json.dumps(team2_ids),
                    winning_team_db,
                    dotabuff_match_id,
                    notes,
                ),
            )

            match_id = cursor.lastrowid

            # Determine win/loss based on winning_team_db (1=Radiant, 2=Dire)
            team1_won = winning_team_db == 1

            # Insert participants with side info
            for player_id in team1_ids:
                side = "radiant"
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 1, ?, ?)
                """,
                    (match_id, player_id, team1_won, side),
                )

            for player_id in team2_ids:
                side = "dire"
                cursor.execute(
                    """
                    INSERT INTO match_participants (match_id, discord_id, team_number, won, side)
                    VALUES (?, ?, 2, ?, ?)
                """,
                    (match_id, player_id, not team1_won, side),
                )

            # Update player win/loss counts
            for player_id in team1_ids:
                if team1_won:
                    cursor.execute(
                        """
                        UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ?
                    """,
                        (player_id,),
                    )
                else:
                    cursor.execute(
                        """
                        UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ?
                    """,
                        (player_id,),
                    )

            for player_id in team2_ids:
                if not team1_won:
                    cursor.execute(
                        """
                        UPDATE players SET wins = wins + 1, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ?
                    """,
                        (player_id,),
                    )
                else:
                    cursor.execute(
                        """
                        UPDATE players SET losses = losses + 1, updated_at = CURRENT_TIMESTAMP
                        WHERE discord_id = ?
                    """,
                        (player_id,),
                    )

            return match_id

    def update_player_mmr(self, discord_id: int, new_mmr: float, match_id: int | None = None):
        """
        Update player's current MMR and record in history.

        Args:
            discord_id: Discord user ID
            new_mmr: New MMR value
            match_id: Optional match ID that triggered this update
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Update current MMR
            cursor.execute(
                """
                UPDATE players
                SET current_mmr = ?, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (new_mmr, discord_id),
            )

            # Record in history
            cursor.execute(
                """
                INSERT INTO rating_history (discord_id, rating, match_id)
                VALUES (?, ?, ?)
            """,
                (discord_id, new_mmr, match_id),
            )

    def get_exclusion_counts(self, discord_ids: list[int]) -> dict[int, int]:
        """
        Get exclusion counts for multiple players.

        Args:
            discord_ids: List of Discord user IDs

        Returns:
            Dict mapping discord_id to exclusion_count
        """
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

    def increment_exclusion_count(self, discord_id: int):
        """
        Increment a player's exclusion count by 4.
        Called when a player is excluded from a match.

        Args:
            discord_id: Discord user ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 4, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def increment_exclusion_count_half(self, discord_id: int):
        """
        Increment a player's exclusion count by 2 (half the normal bonus).
        Called for conditional players who weren't picked.

        Args:
            discord_id: Discord user ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) + 2, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def decay_exclusion_count(self, discord_id: int):
        """
        Decay a player's exclusion count by halving it (rounded down).
        Called when a player is included in a match.

        Args:
            discord_id: Discord user ID
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            cursor.execute(
                """
                UPDATE players
                SET exclusion_count = COALESCE(exclusion_count, 0) / 2, updated_at = CURRENT_TIMESTAMP
                WHERE discord_id = ?
            """,
                (discord_id,),
            )

    def delete_player(self, discord_id: int) -> bool:
        """
        Delete a player from the database (removes all their data).

        Args:
            discord_id: Discord user ID

        Returns:
            True if player was deleted, False if player didn't exist
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Check if player exists
            cursor.execute("SELECT discord_id FROM players WHERE discord_id = ?", (discord_id,))
            if not cursor.fetchone():
                return False

            # Delete player (cascade will handle related records if foreign keys are set up)
            cursor.execute("DELETE FROM players WHERE discord_id = ?", (discord_id,))

            # Also delete from match_participants (if foreign key cascade isn't set up)
            cursor.execute("DELETE FROM match_participants WHERE discord_id = ?", (discord_id,))

            # Delete rating history
            cursor.execute("DELETE FROM rating_history WHERE discord_id = ?", (discord_id,))

            return True

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

    def clear_all_players(self) -> int:
        """
        Delete all players from the database (for testing purposes).

        Returns:
            Number of players deleted
        """
        with self.connection() as conn:
            cursor = conn.cursor()

            # Count players before deletion
            cursor.execute("SELECT COUNT(*) FROM players")
            count = cursor.fetchone()[0]

            # Delete all players and related data
            cursor.execute("DELETE FROM players")
            cursor.execute("DELETE FROM match_participants")
            cursor.execute("DELETE FROM rating_history")
            cursor.execute("DELETE FROM match_predictions")
            cursor.execute("DELETE FROM matches")

            return count

    def save_pending_match(self, guild_id: int | None, payload: dict) -> None:
        """
        Save pending match state for a guild.
        """
        normalized = self._normalize_guild_id(guild_id)
        serialized = json.dumps(payload)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO pending_matches (guild_id, payload)
                VALUES (?, ?)
                ON CONFLICT(guild_id) DO UPDATE SET payload = excluded.payload, updated_at = CURRENT_TIMESTAMP
            """,
                (normalized, serialized),
            )

    def get_pending_match(self, guild_id: int | None) -> dict | None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            return json.loads(row["payload"])

    def clear_pending_match(self, guild_id: int | None) -> None:
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))

    def consume_pending_match(self, guild_id: int | None) -> dict | None:
        """
        Atomically retrieve and delete the pending match for a guild.

        Returns the pending match payload if it exists, None otherwise.
        This ensures only one caller can successfully consume a given match.
        """
        normalized = self._normalize_guild_id(guild_id)
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT payload FROM pending_matches WHERE guild_id = ?", (normalized,))
            row = cursor.fetchone()
            if not row:
                return None
            cursor.execute("DELETE FROM pending_matches WHERE guild_id = ?", (normalized,))
            return json.loads(row["payload"])

    def save_lobby_state(
        self,
        lobby_id: int,
        players: list[int],
        status: str,
        created_by: int,
        created_at: str,
        message_id: int | None = None,
        channel_id: int | None = None,
        thread_id: int | None = None,
        embed_message_id: int | None = None,
        conditional_players: list[int] | None = None,
        origin_channel_id: int | None = None,
        player_join_times: dict[int, float] | None = None,
    ) -> None:
        payload = json.dumps(players)
        conditional_payload = json.dumps(conditional_players or [])
        join_times_payload = json.dumps(
            {str(k): v for k, v in (player_join_times or {}).items()}
        )
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute(
                """
                INSERT INTO lobby_state (lobby_id, players, conditional_players, status, created_by, created_at,
                                         message_id, channel_id, thread_id, embed_message_id, origin_channel_id,
                                         player_join_times)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(lobby_id) DO UPDATE SET
                    players = excluded.players,
                    conditional_players = excluded.conditional_players,
                    status = excluded.status,
                    created_by = excluded.created_by,
                    created_at = excluded.created_at,
                    message_id = excluded.message_id,
                    channel_id = excluded.channel_id,
                    thread_id = excluded.thread_id,
                    embed_message_id = excluded.embed_message_id,
                    origin_channel_id = excluded.origin_channel_id,
                    player_join_times = excluded.player_join_times,
                    updated_at = CURRENT_TIMESTAMP
            """,
                (lobby_id, payload, conditional_payload, status, created_by, created_at, message_id, channel_id,
                 thread_id, embed_message_id, origin_channel_id, join_times_payload),
            )

    def load_lobby_state(self, lobby_id: int) -> dict | None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("SELECT * FROM lobby_state WHERE lobby_id = ?", (lobby_id,))
            row = cursor.fetchone()
            if not row:
                return None
            row_dict = dict(row)
            raw_join_times = row_dict.get("player_join_times", "{}")
            join_times = json.loads(raw_join_times) if raw_join_times else {}
            return {
                "lobby_id": row_dict["lobby_id"],
                "players": json.loads(row_dict["players"]) if row_dict.get("players") else [],
                "conditional_players": json.loads(row_dict["conditional_players"]) if row_dict.get("conditional_players") else [],
                "player_join_times": {int(k): v for k, v in join_times.items()},
                "status": row_dict["status"],
                "created_by": row_dict["created_by"],
                "created_at": row_dict["created_at"],
                "message_id": row_dict.get("message_id"),
                "channel_id": row_dict.get("channel_id"),
                "thread_id": row_dict.get("thread_id"),
                "embed_message_id": row_dict.get("embed_message_id"),
                "origin_channel_id": row_dict.get("origin_channel_id"),
            }

    def clear_lobby_state(self, lobby_id: int) -> None:
        with self.connection() as conn:
            cursor = conn.cursor()
            cursor.execute("DELETE FROM lobby_state WHERE lobby_id = ?", (lobby_id,))

    def _normalize_guild_id(self, guild_id: int | None) -> int:
        return guild_id if guild_id is not None else 0
