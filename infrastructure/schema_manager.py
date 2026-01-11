"""
Schema and migration management for SQLite database.
"""

import logging
import sqlite3

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
                rating_before REAL,
                rd_before REAL,
                rd_after REAL,
                volatility_before REAL,
                volatility_after REAL,
                expected_team_win_prob REAL,
                team_number INTEGER,
                won BOOLEAN,
                match_id INTEGER,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id),
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            )
            """
        )

        # Match prediction snapshots (pre-match)
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS match_predictions (
                match_id INTEGER PRIMARY KEY,
                radiant_rating REAL,
                dire_rating REAL,
                radiant_rd REAL,
                dire_rd REAL,
                expected_radiant_win_prob REAL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
            (
                "recreate_bets_table_with_guild_id",
                self._migration_recreate_bets_table_with_guild_id,
            ),
            ("add_indexes_v1", self._migration_add_indexes_v1),
            ("add_bet_leverage_column", self._migration_add_bet_leverage_column),
            ("create_player_pairings_table", self._migration_create_player_pairings_table),
            ("create_guild_config_table", self._migration_create_guild_config_table),
            ("add_steam_id_to_players", self._migration_add_steam_id_to_players),
            ("add_match_enrichment_columns", self._migration_add_match_enrichment_columns),
            ("add_enrichment_source_columns", self._migration_add_enrichment_source_columns),
            ("create_bankruptcy_table", self._migration_create_bankruptcy_table),
            ("add_lobby_message_columns", self._migration_add_lobby_message_columns),
            ("add_participant_healing_lane_columns", self._migration_add_participant_healing_lane),
            ("add_lane_efficiency_column", self._migration_add_lane_efficiency),
            ("add_bet_payout_column", self._migration_add_bet_payout_column),
            ("create_loan_system", self._migration_create_loan_system),
            ("add_negative_loans_column", self._migration_add_negative_loans_column),
            ("add_outstanding_loan_columns", self._migration_add_outstanding_loan_columns),
            ("create_disburse_system", self._migration_create_disburse_system),
            ("add_rating_history_details", self._migration_add_rating_history_details),
            ("create_match_predictions_table", self._migration_create_match_predictions_table),
            ("create_predictions_system", self._migration_create_predictions_system),
            ("add_prediction_channel_message_id", self._migration_add_prediction_channel_message_id),
            ("add_last_match_date_to_players", self._migration_add_last_match_date_to_players),
            ("add_bet_is_blind_column", self._migration_add_bet_is_blind_column),
            ("add_bet_odds_at_placement_column", self._migration_add_bet_odds_at_placement_column),
            ("add_lobby_thread_columns", self._migration_add_lobby_thread_columns),
            ("add_ai_features_enabled", self._migration_add_ai_features_enabled),
            ("add_bankruptcy_count_column", self._migration_add_bankruptcy_count_column),
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
        cursor.execute("CREATE INDEX IF NOT EXISTS idx_matches_match_date ON matches(match_date)")
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_guild_match_bet_time ON bets(guild_id, match_id, bet_time)"
        )

    def _migration_add_bet_leverage_column(self, cursor) -> None:
        """Add leverage column to bets table for leverage betting."""
        self._add_column_if_not_exists(cursor, "bets", "leverage", "INTEGER DEFAULT 1")

    def _migration_create_player_pairings_table(self, cursor) -> None:
        """Create table for pairwise player statistics."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_pairings (
                player1_id INTEGER NOT NULL,
                player2_id INTEGER NOT NULL,
                games_together INTEGER DEFAULT 0,
                wins_together INTEGER DEFAULT 0,
                games_against INTEGER DEFAULT 0,
                player1_wins_against INTEGER DEFAULT 0,
                last_match_id INTEGER,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (player1_id, player2_id),
                FOREIGN KEY (player1_id) REFERENCES players(discord_id),
                FOREIGN KEY (player2_id) REFERENCES players(discord_id),
                FOREIGN KEY (last_match_id) REFERENCES matches(match_id),
                CHECK (player1_id < player2_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pairings_player1 ON player_pairings(player1_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pairings_player2 ON player_pairings(player2_id)"
        )

    def _migration_create_guild_config_table(self, cursor) -> None:
        """Create table for per-guild configuration."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS guild_config (
                guild_id INTEGER PRIMARY KEY,
                league_id INTEGER,
                auto_enrich_matches INTEGER DEFAULT 1,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _migration_add_steam_id_to_players(self, cursor) -> None:
        """Add steam_id column for direct Valve API correlation."""
        self._add_column_if_not_exists(cursor, "players", "steam_id", "INTEGER")
        cursor.execute("CREATE INDEX IF NOT EXISTS idx_players_steam_id ON players(steam_id)")

    def _migration_add_match_enrichment_columns(self, cursor) -> None:
        """Add columns for Valve API match enrichment."""
        # Match-level enrichment
        self._add_column_if_not_exists(cursor, "matches", "valve_match_id", "INTEGER")
        self._add_column_if_not_exists(cursor, "matches", "duration_seconds", "INTEGER")
        self._add_column_if_not_exists(cursor, "matches", "radiant_score", "INTEGER")
        self._add_column_if_not_exists(cursor, "matches", "dire_score", "INTEGER")
        self._add_column_if_not_exists(cursor, "matches", "game_mode", "INTEGER")
        self._add_column_if_not_exists(cursor, "matches", "enrichment_data", "TEXT")

        # Per-participant enrichment
        self._add_column_if_not_exists(cursor, "match_participants", "hero_id", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "kills", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "deaths", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "assists", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "last_hits", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "denies", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "gpm", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "xpm", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "hero_damage", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "tower_damage", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "net_worth", "INTEGER")

    def _migration_add_enrichment_source_columns(self, cursor) -> None:
        """Add columns to track enrichment source (manual vs auto-discovered)."""
        # 'manual' = user ran /enrichmatch, 'auto' = discovered by /autodiscover
        self._add_column_if_not_exists(cursor, "matches", "enrichment_source", "TEXT")
        # Confidence score for auto-discovered matches (0.0 - 1.0)
        self._add_column_if_not_exists(cursor, "matches", "enrichment_confidence", "REAL")

    def _migration_create_bankruptcy_table(self, cursor) -> None:
        """Create table for tracking bankruptcy cooldowns and penalties."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS bankruptcy_state (
                discord_id INTEGER PRIMARY KEY,
                last_bankruptcy_at INTEGER,
                penalty_games_remaining INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

    def _migration_add_lobby_message_columns(self, cursor) -> None:
        """Add message_id and channel_id columns to lobby_state for persistence across restarts."""
        self._add_column_if_not_exists(cursor, "lobby_state", "message_id", "INTEGER")
        self._add_column_if_not_exists(cursor, "lobby_state", "channel_id", "INTEGER")

    def _migration_add_participant_healing_lane(self, cursor) -> None:
        """Add hero_healing and lane_role columns for enhanced match stats."""
        self._add_column_if_not_exists(cursor, "match_participants", "hero_healing", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "lane_role", "INTEGER")

    def _migration_add_lane_efficiency(self, cursor) -> None:
        """Add lane_efficiency column for laning phase performance (0-100)."""
        self._add_column_if_not_exists(cursor, "match_participants", "lane_efficiency", "INTEGER")

    def _migration_add_bet_payout_column(self, cursor) -> None:
        """Add payout column to bets and backfill historical data assuming house mode."""
        self._add_column_if_not_exists(cursor, "bets", "payout", "INTEGER")

        # Backfill historical settled bets with payout values
        # Winners get: amount * leverage * 2 (stake returned + equal profit in house mode)
        # Losers keep payout as NULL
        cursor.execute(
            """
            UPDATE bets
            SET payout = amount * COALESCE(leverage, 1) * 2
            WHERE match_id IS NOT NULL
            AND payout IS NULL
            AND bet_id IN (
                SELECT b.bet_id FROM bets b
                JOIN matches m ON b.match_id = m.match_id
                WHERE (m.winning_team = 1 AND b.team_bet_on = 'radiant')
                   OR (m.winning_team = 2 AND b.team_bet_on = 'dire')
            )
            """
        )

    def _migration_create_loan_system(self, cursor) -> None:
        """Create tables for loan system and lowest balance tracking."""
        # Loan state table (similar to bankruptcy_state)
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS loan_state (
                discord_id INTEGER PRIMARY KEY,
                last_loan_at INTEGER,
                total_loans_taken INTEGER DEFAULT 0,
                total_fees_paid INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

        # Nonprofit fund for gambling addiction (collects loan fees)
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS nonprofit_fund (
                guild_id INTEGER PRIMARY KEY DEFAULT 0,
                total_collected INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

        # Add lowest_balance_ever to players for credit/degen scoring
        self._add_column_if_not_exists(cursor, "players", "lowest_balance_ever", "INTEGER")

    def _migration_add_negative_loans_column(self, cursor) -> None:
        """Track loans taken while already in debt (peak degen behavior)."""
        self._add_column_if_not_exists(
            cursor, "loan_state", "negative_loans_taken", "INTEGER DEFAULT 0"
        )

    def _migration_add_outstanding_loan_columns(self, cursor) -> None:
        """Track outstanding loan principal and fee for deferred repayment."""
        self._add_column_if_not_exists(
            cursor, "loan_state", "outstanding_principal", "INTEGER DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "loan_state", "outstanding_fee", "INTEGER DEFAULT 0"
        )

    def _migration_create_disburse_system(self, cursor) -> None:
        """Create tables for nonprofit fund disbursement voting system."""
        # Active proposal per guild
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS disburse_proposals (
                guild_id INTEGER PRIMARY KEY,
                proposal_id INTEGER NOT NULL,
                message_id INTEGER,
                channel_id INTEGER,
                fund_amount INTEGER NOT NULL,
                quorum_required INTEGER NOT NULL,
                status TEXT DEFAULT 'active',
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

        # Vote tracking
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS disburse_votes (
                guild_id INTEGER NOT NULL,
                proposal_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                vote_method TEXT NOT NULL,
                voted_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, proposal_id, discord_id)
            )
            """
        )

        # Disbursement history (for /nonprofit command)
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS disburse_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                disbursed_at INTEGER NOT NULL,
                total_amount INTEGER NOT NULL,
                method TEXT NOT NULL,
                recipient_count INTEGER NOT NULL,
                recipients TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _migration_add_rating_history_details(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "rating_history", "rating_before", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "rd_before", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "rd_after", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "volatility_before", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "volatility_after", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "expected_team_win_prob", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "team_number", "INTEGER")
        self._add_column_if_not_exists(cursor, "rating_history", "won", "BOOLEAN")

    def _migration_create_match_predictions_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS match_predictions (
                match_id INTEGER PRIMARY KEY,
                radiant_rating REAL,
                dire_rating REAL,
                radiant_rd REAL,
                dire_rd REAL,
                expected_radiant_win_prob REAL,
                timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            )
            """
        )

    def _migration_create_predictions_system(self, cursor) -> None:
        """Create tables for prediction market system (Polymarket-style betting)."""
        # Predictions table
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS predictions (
                prediction_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                creator_id INTEGER NOT NULL,
                question TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'open',
                outcome TEXT,
                channel_id INTEGER,
                thread_id INTEGER,
                embed_message_id INTEGER,
                resolution_votes TEXT,
                created_at INTEGER NOT NULL,
                closes_at INTEGER NOT NULL,
                resolved_at INTEGER,
                resolved_by INTEGER,
                FOREIGN KEY (creator_id) REFERENCES players(discord_id)
            )
            """
        )

        # Prediction bets table
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS prediction_bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                prediction_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                position TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                payout INTEGER,
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

        # Indexes for efficient queries
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_predictions_guild_status "
            "ON predictions(guild_id, status)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_prediction_bets_prediction "
            "ON prediction_bets(prediction_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_prediction_bets_user "
            "ON prediction_bets(discord_id)"
        )

    def _migration_add_prediction_channel_message_id(self, cursor) -> None:
        """Add channel_message_id column to predictions table."""
        self._add_column_if_not_exists(
            cursor, "predictions", "channel_message_id", "INTEGER"
        )
        self._add_column_if_not_exists(
            cursor, "predictions", "close_message_id", "INTEGER"
        )

    def _migration_add_last_match_date_to_players(self, cursor) -> None:
        """
        Add last_match_date to players to track most recent played match.

        Backfill existing players with created_at to avoid NULL where possible.
        """
        self._add_column_if_not_exists(cursor, "players", "last_match_date", "TIMESTAMP")
        # Backfill: if created_at exists, use it; otherwise leave NULL
        cursor.execute(
            """
            UPDATE players
            SET last_match_date = COALESCE(last_match_date, created_at)
            WHERE last_match_date IS NULL
            """
        )

    def _migration_add_bet_is_blind_column(self, cursor) -> None:
        """Add is_blind column to bets table for auto-liquidity blind bets."""
        self._add_column_if_not_exists(cursor, "bets", "is_blind", "INTEGER DEFAULT 0")

    def _migration_add_bet_odds_at_placement_column(self, cursor) -> None:
        """Add odds_at_placement column to bets table for historical odds tracking."""
        self._add_column_if_not_exists(cursor, "bets", "odds_at_placement", "REAL")

    def _migration_add_lobby_thread_columns(self, cursor) -> None:
        """Add thread_id and embed_message_id columns to lobby_state for thread support."""
        self._add_column_if_not_exists(cursor, "lobby_state", "thread_id", "INTEGER")
        self._add_column_if_not_exists(cursor, "lobby_state", "embed_message_id", "INTEGER")

    def _migration_add_ai_features_enabled(self, cursor) -> None:
        """Add ai_features_enabled column to guild_config for AI feature toggle."""
        self._add_column_if_not_exists(cursor, "guild_config", "ai_features_enabled", "INTEGER DEFAULT 0")

    def _migration_add_bankruptcy_count_column(self, cursor) -> None:
        """Add bankruptcy_count column to bankruptcy_state to track total bankruptcies."""
        self._add_column_if_not_exists(cursor, "bankruptcy_state", "bankruptcy_count", "INTEGER DEFAULT 0")
        # Backfill: if last_bankruptcy_at is set but bankruptcy_count is 0, set to 1
        cursor.execute(
            """
            UPDATE bankruptcy_state
            SET bankruptcy_count = 1
            WHERE last_bankruptcy_at IS NOT NULL AND (bankruptcy_count IS NULL OR bankruptcy_count = 0)
            """
        )
