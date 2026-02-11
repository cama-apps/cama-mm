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
        if not self.use_uri:  # Skip WAL for in-memory databases
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA busy_timeout=5000")
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
            ("create_recalibration_state_table", self._migration_create_recalibration_state_table),
            ("add_first_calibrated_at_to_players", self._migration_add_first_calibrated_at_to_players),
            ("add_captain_eligible_column", self._migration_add_captain_eligible_column),
            ("add_lobby_type_column", self._migration_add_lobby_type_column),
            ("create_player_stakes_table", self._migration_create_player_stakes_table),
            ("create_spectator_bets_table", self._migration_create_spectator_bets_table),
            ("create_player_pool_bets_table", self._migration_create_player_pool_bets_table),
            ("add_conditional_players_to_lobby", self._migration_add_conditional_players_to_lobby),
            ("add_leaderboard_performance_indexes", self._migration_add_leaderboard_performance_indexes),
            ("add_fantasy_columns", self._migration_add_fantasy_columns),
            ("add_openskill_columns", self._migration_add_openskill_columns),
            ("create_tip_transactions_table", self._migration_create_tip_transactions_table),
            ("add_origin_channel_id_to_lobby", self._migration_add_origin_channel_id_to_lobby),
            ("add_last_wheel_spin_to_players", self._migration_add_last_wheel_spin_to_players),
            ("create_wheel_spins_table", self._migration_create_wheel_spins_table),
            ("add_balancing_rating_system_column", self._migration_add_balancing_rating_system_column),
            ("create_match_corrections_table", self._migration_create_match_corrections_table),
            ("create_player_steam_ids_table", self._migration_create_player_steam_ids_table),
            ("add_streak_columns_to_rating_history", self._migration_add_streak_columns),
            ("create_double_or_nothing_table", self._migration_create_double_or_nothing_table),
            ("add_last_double_or_nothing_column", self._migration_add_last_double_or_nothing),
            ("create_wrapped_generation_table", self._migration_create_wrapped_generation_table),
            # Guild isolation migrations (Phase 1)
            ("add_guild_id_to_players", self._migration_add_guild_id_to_players),
            ("add_guild_id_to_matches", self._migration_add_guild_id_to_matches),
            ("add_guild_id_to_match_participants", self._migration_add_guild_id_to_match_participants),
            ("add_guild_id_to_rating_history", self._migration_add_guild_id_to_rating_history),
            ("add_guild_id_to_player_pairings", self._migration_add_guild_id_to_player_pairings),
            ("add_guild_id_to_loan_state", self._migration_add_guild_id_to_loan_state),
            ("add_guild_id_to_bankruptcy_state", self._migration_add_guild_id_to_bankruptcy_state),
            ("add_guild_id_to_recalibration_state", self._migration_add_guild_id_to_recalibration_state),
            # Soft avoid feature
            ("create_soft_avoids_table", self._migration_create_soft_avoids_table),
            # Ready check join times
            ("add_player_join_times_to_lobby", self._migration_add_player_join_times_to_lobby),
            # Easter egg tracking columns
            ("add_easter_egg_tracking_columns", self._migration_add_easter_egg_tracking_columns),
            ("create_neon_events_table", self._migration_create_neon_events_table),
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

    def _migration_create_recalibration_state_table(self, cursor) -> None:
        """Create table for tracking recalibration history and cooldowns."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS recalibration_state (
                discord_id INTEGER PRIMARY KEY,
                last_recalibration_at INTEGER,
                total_recalibrations INTEGER DEFAULT 0,
                rating_at_recalibration REAL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

    def _migration_add_first_calibrated_at_to_players(self, cursor) -> None:
        """Add first_calibrated_at column to players and backfill for calibrated players."""
        self._add_column_if_not_exists(cursor, "players", "first_calibrated_at", "INTEGER")
        # Backfill: for players with RD <= 100 (calibrated), use created_at as approximation
        cursor.execute(
            """
            UPDATE players
            SET first_calibrated_at = CAST(strftime('%s', created_at) AS INTEGER)
            WHERE glicko_rd IS NOT NULL AND glicko_rd <= 100.0 AND first_calibrated_at IS NULL
            """
        )

    def _migration_add_captain_eligible_column(self, cursor) -> None:
        """Add is_captain_eligible column to players for Immortal Draft mode."""
        self._add_column_if_not_exists(cursor, "players", "is_captain_eligible", "INTEGER DEFAULT 0")

    def _migration_add_lobby_type_column(self, cursor) -> None:
        """Add lobby_type column to matches for tracking shuffle vs draft mode."""
        self._add_column_if_not_exists(cursor, "matches", "lobby_type", "TEXT DEFAULT 'shuffle'")

    def _migration_create_player_stakes_table(self, cursor) -> None:
        """Create table for player stake pool in draft mode."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_stakes (
                stake_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                is_excluded INTEGER DEFAULT 0,
                payout INTEGER,
                stake_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_stakes_guild_match "
            "ON player_stakes(guild_id, match_id, stake_time)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_stakes_discord "
            "ON player_stakes(discord_id)"
        )

    def _migration_create_spectator_bets_table(self, cursor) -> None:
        """Create table for spectator pool bets (parimutuel with player cut)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS spectator_bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                payout INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_spectator_bets_guild_match "
            "ON spectator_bets(guild_id, match_id, bet_time)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_spectator_bets_discord "
            "ON spectator_bets(discord_id)"
        )

    def _migration_create_player_pool_bets_table(self, cursor) -> None:
        """Create table for player pool bets (real JC bets by match participants)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_pool_bets (
                bet_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team TEXT NOT NULL,
                amount INTEGER NOT NULL,
                bet_time INTEGER NOT NULL,
                payout INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pool_bets_guild_match "
            "ON player_pool_bets(guild_id, match_id, bet_time)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pool_bets_discord "
            "ON player_pool_bets(discord_id)"
        )

    def _migration_add_conditional_players_to_lobby(self, cursor) -> None:
        """Add conditional_players column to lobby_state for frogling players."""
        self._add_column_if_not_exists(
            cursor, "lobby_state", "conditional_players", "TEXT DEFAULT '[]'"
        )

    def _migration_add_leaderboard_performance_indexes(self, cursor) -> None:
        """Add indexes to improve leaderboard query performance."""
        # Index for filtering bets by discord_id (used in gambling stats)
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_discord_id ON bets(discord_id)"
        )
        # Composite index for guild + discord_id lookups
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_guild_discord ON bets(guild_id, discord_id)"
        )
        # Index for player leaderboard sorting (jopacoin, wins)
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_players_leaderboard "
            "ON players(jopacoin_balance DESC, wins DESC, glicko_rating DESC)"
        )

    def _migration_add_fantasy_columns(self, cursor) -> None:
        """Add fantasy scoring columns to match_participants for OpenDota enrichment."""
        # Tower/Roshan objectives
        self._add_column_if_not_exists(cursor, "match_participants", "towers_killed", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "roshans_killed", "INTEGER")
        # Teamfight participation (0.0 - 1.0)
        self._add_column_if_not_exists(cursor, "match_participants", "teamfight_participation", "REAL")
        # Vision game
        self._add_column_if_not_exists(cursor, "match_participants", "obs_placed", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "sen_placed", "INTEGER")
        # Jungle/economy
        self._add_column_if_not_exists(cursor, "match_participants", "camps_stacked", "INTEGER")
        self._add_column_if_not_exists(cursor, "match_participants", "rune_pickups", "INTEGER")
        # Early game
        self._add_column_if_not_exists(cursor, "match_participants", "firstblood_claimed", "INTEGER")
        # Crowd control (stun duration in seconds)
        self._add_column_if_not_exists(cursor, "match_participants", "stuns", "REAL")
        # Calculated fantasy points
        self._add_column_if_not_exists(cursor, "match_participants", "fantasy_points", "REAL")

    def _migration_add_openskill_columns(self, cursor) -> None:
        """Add OpenSkill Plackett-Luce rating columns to players and rating_history."""
        # Player-level OpenSkill ratings (mu, sigma)
        self._add_column_if_not_exists(cursor, "players", "os_mu", "REAL")
        self._add_column_if_not_exists(cursor, "players", "os_sigma", "REAL")

        # Rating history for OpenSkill tracking
        self._add_column_if_not_exists(cursor, "rating_history", "os_mu_before", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "os_mu_after", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "os_sigma_before", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "os_sigma_after", "REAL")
        self._add_column_if_not_exists(cursor, "rating_history", "fantasy_weight", "REAL")

    def _migration_create_tip_transactions_table(self, cursor) -> None:
        """Create table for tracking tip transactions with fees."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS tip_transactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sender_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                amount INTEGER NOT NULL,
                fee INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                timestamp INTEGER NOT NULL,
                FOREIGN KEY (sender_id) REFERENCES players(discord_id),
                FOREIGN KEY (recipient_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_tip_transactions_sender ON tip_transactions(sender_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_tip_transactions_recipient ON tip_transactions(recipient_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_tip_transactions_timestamp ON tip_transactions(timestamp)"
        )

    def _migration_add_origin_channel_id_to_lobby(self, cursor) -> None:
        """Add origin_channel_id column to lobby_state for dedicated lobby channel support."""
        self._add_column_if_not_exists(cursor, "lobby_state", "origin_channel_id", "INTEGER")

    def _migration_add_last_wheel_spin_to_players(self, cursor) -> None:
        """Add last_wheel_spin column to players for persisting gamba cooldown."""
        self._add_column_if_not_exists(cursor, "players", "last_wheel_spin", "INTEGER")

    def _migration_create_wheel_spins_table(self, cursor) -> None:
        """Create wheel_spins table to track /gamba results for gambachart."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS wheel_spins (
                spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                result INTEGER NOT NULL,
                spin_time INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_wheel_spins_discord_id ON wheel_spins(discord_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_wheel_spins_spin_time ON wheel_spins(spin_time)"
        )

    def _migration_add_balancing_rating_system_column(self, cursor) -> None:
        """Track which rating system was used for team balancing (experiment)."""
        self._add_column_if_not_exists(
            cursor, "matches", "balancing_rating_system", "TEXT DEFAULT 'glicko'"
        )

    def _migration_create_match_corrections_table(self, cursor) -> None:
        """Create table for tracking match result corrections."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS match_corrections (
                correction_id INTEGER PRIMARY KEY AUTOINCREMENT,
                match_id INTEGER NOT NULL,
                old_winning_team INTEGER NOT NULL,
                new_winning_team INTEGER NOT NULL,
                corrected_by INTEGER NOT NULL,
                corrected_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (match_id) REFERENCES matches(match_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_match_corrections_match_id ON match_corrections(match_id)"
        )

    def _migration_create_player_steam_ids_table(self, cursor) -> None:
        """Create junction table for multiple Steam IDs per player."""
        # Create the junction table
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_steam_ids (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                steam_id INTEGER NOT NULL,
                is_primary INTEGER DEFAULT 0,
                added_at INTEGER NOT NULL,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id) ON DELETE CASCADE,
                UNIQUE (discord_id, steam_id),
                UNIQUE (steam_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_steam_ids_discord ON player_steam_ids(discord_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_steam_ids_steam ON player_steam_ids(steam_id)"
        )

        # Migrate existing steam_ids from players table to junction table
        # Only migrate non-null steam_ids that don't already exist in the junction table
        cursor.execute(
            """
            INSERT OR IGNORE INTO player_steam_ids (discord_id, steam_id, is_primary, added_at)
            SELECT discord_id, steam_id, 1, CAST(strftime('%s', 'now') AS INTEGER)
            FROM players
            WHERE steam_id IS NOT NULL
            """
        )

    def _migration_add_streak_columns(self, cursor) -> None:
        """Add streak tracking columns to rating_history for analytics."""
        self._add_column_if_not_exists(cursor, "rating_history", "streak_length", "INTEGER")
        self._add_column_if_not_exists(cursor, "rating_history", "streak_multiplier", "REAL")

    def _migration_create_double_or_nothing_table(self, cursor) -> None:
        """Create table for tracking Double or Nothing spin history."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS double_or_nothing_spins (
                spin_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                cost INTEGER NOT NULL,
                balance_before INTEGER NOT NULL,
                balance_after INTEGER NOT NULL,
                won INTEGER NOT NULL,
                spin_time INTEGER NOT NULL,
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_don_discord_id ON double_or_nothing_spins(discord_id)"
        )

    def _migration_add_last_double_or_nothing(self, cursor) -> None:
        """Add last_double_or_nothing column to players for cooldown tracking."""
        self._add_column_if_not_exists(cursor, "players", "last_double_or_nothing", "INTEGER")

    def _migration_create_wrapped_generation_table(self, cursor) -> None:
        """Create table for tracking monthly wrapped generation."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS wrapped_generation (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                year_month TEXT NOT NULL,
                channel_id INTEGER,
                message_id INTEGER,
                generated_at INTEGER NOT NULL,
                generated_by INTEGER,
                generation_type TEXT DEFAULT 'auto',
                stats_json TEXT,
                UNIQUE (guild_id, year_month)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_wrapped_guild_month ON wrapped_generation(guild_id, year_month)"
        )

    # =========================================================================
    # Guild Isolation Migrations
    # These migrations add guild_id to tables to support multi-server isolation.
    # Existing data is assigned to the original server (806299990791159808).
    # =========================================================================

    # Hardcoded guild ID for migrating existing data (one-time migration)
    _LEGACY_GUILD_ID = 806299990791159808

    def _migration_add_guild_id_to_players(self, cursor) -> None:
        """
        Add guild_id to players table, changing to composite primary key.

        SQLite doesn't support altering primary keys, so we recreate the table.
        Existing players are assigned to the legacy guild.
        """
        # Check if guild_id column already exists
        cursor.execute("PRAGMA table_info(players)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        # Create new table with composite primary key
        cursor.execute(
            """
            CREATE TABLE players_new (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
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
                os_mu REAL,
                os_sigma REAL,
                steam_id INTEGER,
                jopacoin_balance INTEGER DEFAULT 3,
                exclusion_count INTEGER DEFAULT 0,
                lowest_balance_ever INTEGER,
                last_match_date TIMESTAMP,
                first_calibrated_at INTEGER,
                is_captain_eligible INTEGER DEFAULT 0,
                last_wheel_spin INTEGER,
                last_double_or_nothing INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

        # Copy existing data with legacy guild_id
        cursor.execute(
            f"""
            INSERT INTO players_new (
                discord_id, guild_id, discord_username, dotabuff_url, initial_mmr,
                current_mmr, wins, losses, preferred_roles, main_role,
                glicko_rating, glicko_rd, glicko_volatility, os_mu, os_sigma,
                steam_id, jopacoin_balance, exclusion_count, lowest_balance_ever,
                last_match_date, first_calibrated_at, is_captain_eligible,
                last_wheel_spin, last_double_or_nothing, created_at, updated_at
            )
            SELECT
                discord_id, {self._LEGACY_GUILD_ID}, discord_username, dotabuff_url, initial_mmr,
                current_mmr, wins, losses, preferred_roles, main_role,
                glicko_rating, glicko_rd, glicko_volatility, os_mu, os_sigma,
                steam_id, COALESCE(jopacoin_balance, 3), COALESCE(exclusion_count, 0),
                lowest_balance_ever, last_match_date, first_calibrated_at,
                COALESCE(is_captain_eligible, 0), last_wheel_spin, last_double_or_nothing,
                created_at, updated_at
            FROM players
            """
        )

        # Drop old table and rename
        cursor.execute("DROP TABLE players")
        cursor.execute("ALTER TABLE players_new RENAME TO players")

        # Recreate indexes
        cursor.execute("CREATE INDEX IF NOT EXISTS idx_players_steam_id ON players(steam_id)")
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_players_guild_id ON players(guild_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_players_leaderboard "
            "ON players(guild_id, jopacoin_balance DESC, wins DESC, glicko_rating DESC)"
        )

    def _migration_add_guild_id_to_matches(self, cursor) -> None:
        """Add guild_id column to matches table."""
        self._add_column_if_not_exists(cursor, "matches", "guild_id", "INTEGER NOT NULL DEFAULT 0")

        # Update existing matches with legacy guild_id
        cursor.execute(
            f"UPDATE matches SET guild_id = {self._LEGACY_GUILD_ID} WHERE guild_id = 0"
        )

        # Add index for guild-filtered queries
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_matches_guild_id ON matches(guild_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_matches_guild_date ON matches(guild_id, match_date DESC)"
        )

    def _migration_add_guild_id_to_match_participants(self, cursor) -> None:
        """Add guild_id column to match_participants table."""
        self._add_column_if_not_exists(
            cursor, "match_participants", "guild_id", "INTEGER NOT NULL DEFAULT 0"
        )

        # Update existing participants with legacy guild_id
        cursor.execute(
            f"UPDATE match_participants SET guild_id = {self._LEGACY_GUILD_ID} WHERE guild_id = 0"
        )

        # Add index
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_match_participants_guild "
            "ON match_participants(guild_id, discord_id)"
        )

    def _migration_add_guild_id_to_rating_history(self, cursor) -> None:
        """Add guild_id column to rating_history table."""
        self._add_column_if_not_exists(
            cursor, "rating_history", "guild_id", "INTEGER NOT NULL DEFAULT 0"
        )

        # Update existing history with legacy guild_id
        cursor.execute(
            f"UPDATE rating_history SET guild_id = {self._LEGACY_GUILD_ID} WHERE guild_id = 0"
        )

        # Add index
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_rating_history_guild "
            "ON rating_history(guild_id, discord_id)"
        )

    def _migration_add_guild_id_to_player_pairings(self, cursor) -> None:
        """
        Add guild_id to player_pairings table, changing to composite primary key.

        New key: (guild_id, player1_id, player2_id)
        """
        # Check if guild_id column already exists
        cursor.execute("PRAGMA table_info(player_pairings)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        # Create new table with composite primary key including guild_id
        cursor.execute(
            """
            CREATE TABLE player_pairings_new (
                guild_id INTEGER NOT NULL DEFAULT 0,
                player1_id INTEGER NOT NULL,
                player2_id INTEGER NOT NULL,
                games_together INTEGER DEFAULT 0,
                wins_together INTEGER DEFAULT 0,
                games_against INTEGER DEFAULT 0,
                player1_wins_against INTEGER DEFAULT 0,
                last_match_id INTEGER,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (guild_id, player1_id, player2_id),
                CHECK (player1_id < player2_id)
            )
            """
        )

        # Copy existing data with legacy guild_id
        cursor.execute(
            f"""
            INSERT INTO player_pairings_new (
                guild_id, player1_id, player2_id, games_together, wins_together,
                games_against, player1_wins_against, last_match_id, updated_at
            )
            SELECT
                {self._LEGACY_GUILD_ID}, player1_id, player2_id, games_together, wins_together,
                games_against, player1_wins_against, last_match_id, updated_at
            FROM player_pairings
            """
        )

        # Drop old table and rename
        cursor.execute("DROP TABLE player_pairings")
        cursor.execute("ALTER TABLE player_pairings_new RENAME TO player_pairings")

        # Recreate indexes
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pairings_guild "
            "ON player_pairings(guild_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pairings_player1 "
            "ON player_pairings(guild_id, player1_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_player_pairings_player2 "
            "ON player_pairings(guild_id, player2_id)"
        )

    def _migration_add_guild_id_to_loan_state(self, cursor) -> None:
        """
        Add guild_id to loan_state table, changing to composite primary key.

        New key: (discord_id, guild_id)
        """
        # Check if guild_id column already exists
        cursor.execute("PRAGMA table_info(loan_state)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        # Create new table with composite primary key
        cursor.execute(
            """
            CREATE TABLE loan_state_new (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_loan_at INTEGER,
                total_loans_taken INTEGER DEFAULT 0,
                total_fees_paid INTEGER DEFAULT 0,
                negative_loans_taken INTEGER DEFAULT 0,
                outstanding_principal INTEGER DEFAULT 0,
                outstanding_fee INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

        # Copy existing data with legacy guild_id
        cursor.execute(
            f"""
            INSERT INTO loan_state_new (
                discord_id, guild_id, last_loan_at, total_loans_taken, total_fees_paid,
                negative_loans_taken, outstanding_principal, outstanding_fee, updated_at
            )
            SELECT
                discord_id, {self._LEGACY_GUILD_ID}, last_loan_at,
                COALESCE(total_loans_taken, 0), COALESCE(total_fees_paid, 0),
                COALESCE(negative_loans_taken, 0), COALESCE(outstanding_principal, 0),
                COALESCE(outstanding_fee, 0), updated_at
            FROM loan_state
            """
        )

        # Drop old table and rename
        cursor.execute("DROP TABLE loan_state")
        cursor.execute("ALTER TABLE loan_state_new RENAME TO loan_state")

        # Add index
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_loan_state_guild ON loan_state(guild_id)"
        )

    def _migration_add_guild_id_to_bankruptcy_state(self, cursor) -> None:
        """
        Add guild_id to bankruptcy_state table, changing to composite primary key.

        New key: (discord_id, guild_id)
        """
        # Check if guild_id column already exists
        cursor.execute("PRAGMA table_info(bankruptcy_state)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        # Create new table with composite primary key
        cursor.execute(
            """
            CREATE TABLE bankruptcy_state_new (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_bankruptcy_at INTEGER,
                penalty_games_remaining INTEGER DEFAULT 0,
                bankruptcy_count INTEGER DEFAULT 0,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

        # Copy existing data with legacy guild_id
        cursor.execute(
            f"""
            INSERT INTO bankruptcy_state_new (
                discord_id, guild_id, last_bankruptcy_at, penalty_games_remaining,
                bankruptcy_count, updated_at
            )
            SELECT
                discord_id, {self._LEGACY_GUILD_ID}, last_bankruptcy_at,
                COALESCE(penalty_games_remaining, 0), COALESCE(bankruptcy_count, 0), updated_at
            FROM bankruptcy_state
            """
        )

        # Drop old table and rename
        cursor.execute("DROP TABLE bankruptcy_state")
        cursor.execute("ALTER TABLE bankruptcy_state_new RENAME TO bankruptcy_state")

        # Add index
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bankruptcy_state_guild ON bankruptcy_state(guild_id)"
        )

    def _migration_add_guild_id_to_recalibration_state(self, cursor) -> None:
        """
        Add guild_id to recalibration_state table, changing to composite primary key.

        New key: (discord_id, guild_id)
        """
        # Check if guild_id column already exists
        cursor.execute("PRAGMA table_info(recalibration_state)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        # Create new table with composite primary key
        cursor.execute(
            """
            CREATE TABLE recalibration_state_new (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_recalibration_at INTEGER,
                total_recalibrations INTEGER DEFAULT 0,
                rating_at_recalibration REAL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

        # Copy existing data with legacy guild_id
        cursor.execute(
            f"""
            INSERT INTO recalibration_state_new (
                discord_id, guild_id, last_recalibration_at, total_recalibrations,
                rating_at_recalibration, updated_at
            )
            SELECT
                discord_id, {self._LEGACY_GUILD_ID}, last_recalibration_at,
                COALESCE(total_recalibrations, 0), rating_at_recalibration, updated_at
            FROM recalibration_state
            """
        )

        # Drop old table and rename
        cursor.execute("DROP TABLE recalibration_state")
        cursor.execute("ALTER TABLE recalibration_state_new RENAME TO recalibration_state")

        # Add index
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_recalibration_state_guild "
            "ON recalibration_state(guild_id)"
        )

    def _migration_create_soft_avoids_table(self, cursor) -> None:
        """Create table for soft avoid feature."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS soft_avoids (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                avoider_discord_id INTEGER NOT NULL,
                avoided_discord_id INTEGER NOT NULL,
                games_remaining INTEGER NOT NULL DEFAULT 10,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE (guild_id, avoider_discord_id, avoided_discord_id)
            )
            """
        )
        # Index for looking up avoids by avoider
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_soft_avoids_avoider "
            "ON soft_avoids(guild_id, avoider_discord_id)"
        )
        # Index for looking up avoids targeting a player
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_soft_avoids_avoided "
            "ON soft_avoids(guild_id, avoided_discord_id)"
        )
        # Index for efficient expired avoid cleanup
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_soft_avoids_expired "
            "ON soft_avoids(guild_id, games_remaining) WHERE games_remaining <= 0"
        )

    def _migration_add_player_join_times_to_lobby(self, cursor) -> None:
        """Add player_join_times column to lobby_state for ready check join timestamps."""
        self._add_column_if_not_exists(
            cursor, "lobby_state", "player_join_times", "TEXT DEFAULT '{}'"
        )

    def _migration_add_easter_egg_tracking_columns(self, cursor) -> None:
        """Add columns for easter egg event tracking (JOPA-T expansion)."""
        # Track personal best win streak for streak record events
        self._add_column_if_not_exists(
            cursor, "players", "personal_best_win_streak", "INTEGER DEFAULT 0"
        )
        # Track total bets placed for 100 bets milestone
        self._add_column_if_not_exists(
            cursor, "players", "total_bets_placed", "INTEGER DEFAULT 0"
        )
        # Track whether first leverage bet has been used (one-time trigger)
        self._add_column_if_not_exists(
            cursor, "players", "first_leverage_used", "INTEGER DEFAULT 0"
        )

    def _migration_create_neon_events_table(self, cursor) -> None:
        """Create neon_events table for persisting one-time neon triggers and event history."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS neon_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                event_type TEXT NOT NULL,
                layer INTEGER NOT NULL DEFAULT 1,
                one_time INTEGER NOT NULL DEFAULT 0,
                fired_at INTEGER NOT NULL,
                metadata TEXT
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_neon_events_user_event "
            "ON neon_events(discord_id, guild_id, event_type)"
        )
