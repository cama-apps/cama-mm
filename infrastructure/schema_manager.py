"""
Schema and migration management for SQLite database.
"""

import json
import logging
import sqlite3
from typing import Dict, List, Optional

logger = logging.getLogger("cama_bot.schema")


class SchemaManager:
    """
    Owns schema creation and migrations.

    Call initialize() to ensure schema is present and migrations are applied.
    """

    def __init__(self, db_path: str, use_uri: bool = False):
        self.db_path = db_path
        self.use_uri = use_uri

    def initialize(self) -> None:
        """Create base schema and apply migrations."""
        logger.info(f"Initializing database schema: {self.db_path}")
        with self._connect() as conn:
            cursor = conn.cursor()
            self._create_base_schema(cursor)
            self._create_schema_migrations_table(cursor)
            self._run_migrations(cursor)
            conn.commit()

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.db_path, uri=self.use_uri)
        conn.row_factory = sqlite3.Row
        return conn

    def _create_base_schema(self, cursor) -> None:
        # Players table
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS players (
                discord_id INTEGER PRIMARY KEY,
                discord_username TEXT NOT NULL,
                dotabuff_url TEXT,
                initial_mmr INTEGER,
                current_mmr REAL,
                wins INTEGER DEFAULT 0,
                losses INTEGER DEFAULT 0,
                preferred_roles TEXT,
                main_role TEXT,
                glicko_rating REAL,
                glicko_rd REAL,
                glicko_volatility REAL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

        # Matches table
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS matches (
                match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                team1_players TEXT NOT NULL,
                team2_players TEXT NOT NULL,
                winning_team INTEGER,
                match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                dotabuff_match_id TEXT,
                notes TEXT
            )
            """
        )

        # Match participants
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS match_participants (
                match_id INTEGER,
                discord_id INTEGER,
                team_number INTEGER,
                won BOOLEAN,
                side TEXT,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                PRIMARY KEY (match_id, discord_id)
            )
            """
        )

        # Rating history
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS rating_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER,
                rating REAL,
                match_id INTEGER,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id),
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            )
            """
        )

    # --- Migration helpers ---

    def _add_column_if_not_exists(self, cursor, table: str, column: str, column_type: str) -> None:
        try:
            cursor.execute(f"ALTER TABLE {table} ADD COLUMN {column} {column_type}")
        except sqlite3.OperationalError:
            pass

    def _create_schema_migrations_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS schema_migrations (
                name TEXT PRIMARY KEY,
                applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _run_migrations(self, cursor) -> None:
        applied = {row["name"] for row in cursor.execute("SELECT name FROM schema_migrations")}
        for name, action in self._get_migrations():
            if name in applied:
                continue
            logger.info(f"Applying migration: {name}")
            action(cursor)
            cursor.execute(
                "INSERT INTO schema_migrations (name) VALUES (?)",
                (name,),
            )

    def _get_migrations(self):
        return [
            ("add_glicko_columns", self._migration_add_glicko_columns),
            ("add_exclusion_count", self._migration_add_exclusion_count),
            ("add_pending_matches_table", self._migration_create_pending_matches_table),
            ("add_lobby_state_table", self._migration_create_lobby_state_table),
            ("add_match_participants_side", self._migration_add_match_participants_side_column),
            ("add_jopacoin_balance", self._migration_add_jopacoin_balance),
            ("create_bets_table", self._migration_create_bets_table),
            ("recreate_bets_table_with_guild_id", self._migration_recreate_bets_table_with_guild_id),
            ("add_indexes_v1", self._migration_add_indexes_v1),
            ("add_bet_leverage_column", self._migration_add_bet_leverage_column),
        ]

    # --- Migrations ---

    def _migration_add_glicko_columns(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "glicko_rating", "REAL")
        self._add_column_if_not_exists(cursor, "players", "glicko_rd", "REAL")
        self._add_column_if_not_exists(cursor, "players", "glicko_volatility", "REAL")

    def _migration_add_exclusion_count(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "exclusion_count", "INTEGER DEFAULT 0")

    def _migration_create_pending_matches_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS pending_matches (
                guild_id INTEGER PRIMARY KEY,
                payload TEXT NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _migration_create_lobby_state_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS lobby_state (
                lobby_id INTEGER PRIMARY KEY,
                players TEXT,
                status TEXT,
                created_by INTEGER,
                created_at TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _migration_add_match_participants_side_column(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "match_participants", "side", "TEXT")

    def _migration_add_jopacoin_balance(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "jopacoin_balance", "INTEGER DEFAULT 3")

    def _migration_create_bets_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team_bet_on TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

    def _migration_recreate_bets_table_with_guild_id(self, cursor) -> None:
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='bets'")
        if not cursor.fetchone():
            return

        cursor.execute("PRAGMA table_info(bets)")
        existing_cols = {row["name"] for row in cursor.fetchall()}
        if "guild_id" in existing_cols and "bet_time" in existing_cols:
            return

        cursor.execute(
            """
            CREATE TABLE bets_new (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team_bet_on TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

        cursor.execute(
            """
            INSERT INTO bets_new (bet_id, guild_id, match_id, discord_id, team_bet_on, amount, bet_time, created_at)
            SELECT bet_id, 0, match_id, discord_id, team_bet_on, amount,
                   CAST(COALESCE(strftime('%s', created_at), strftime('%s','now')) AS INTEGER), created_at
            FROM bets
            """
        )

        cursor.execute("DROP TABLE bets")
        cursor.execute("ALTER TABLE bets_new RENAME TO bets")

    def _migration_add_indexes_v1(self, cursor) -> None:
        """
        Add indexes to improve query performance for common access patterns.
        Safe to run multiple times due to IF NOT EXISTS.
        """
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_match_participants_match_id ON match_participants(match_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_match_participants_discord_id ON match_participants(discord_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_rating_history_discord_id ON rating_history(discord_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_rating_history_match_id ON rating_history(match_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_matches_match_date ON matches(match_date)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_guild_match_bet_time ON bets(guild_id, match_id, bet_time)"
        )

    def _migration_add_bet_leverage_column(self, cursor) -> None:
        """Add leverage column to bets table for leverage betting."""
        self._add_column_if_not_exists(cursor, "bets", "leverage", "INTEGER DEFAULT 1")

