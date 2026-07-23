"""
Schema and migration management for SQLite database.
"""

import json
import logging
import sqlite3
import time

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
        """Create the base schema and apply pending migrations.

        All migrations pending at the start of the locked pass and their
        ``schema_migrations`` rows commit in one ``BEGIN IMMEDIATE`` batch, or
        the entire pending batch rolls back.
        """
        logger.info(f"Initializing database schema: {self.db_path}")
        conn = self._connect()
        try:
            cursor = conn.cursor()
            self._create_base_schema(cursor)
            self._create_schema_migrations_table(cursor)
            self._run_migrations(cursor)
        finally:
            conn.close()

    def _connect(self) -> sqlite3.Connection:
        # isolation_level=None → autocommit mode. We manage transactions
        # explicitly via BEGIN IMMEDIATE/COMMIT around migrations so that DDL
        # inside a migration body does not implicitly commit the transaction
        # (Python's default legacy isolation mode commits before DDL).
        conn = sqlite3.connect(self.db_path, uri=self.use_uri, isolation_level=None)
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
        # table/column/column_type are internal migration strings only — not for external input.
        cursor.execute(f"PRAGMA table_info({table})")
        existing = {row["name"] for row in cursor.fetchall()}
        if column in existing:
            return
        cursor.execute(f"ALTER TABLE {table} ADD COLUMN {column} {column_type}")

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
        """Apply the locked pass's pending migrations as one atomic batch.

        Every migration action and its ``schema_migrations`` row commits
        together, or a failure rolls back the entire pending batch.
        """
        applied = {row["name"] for row in cursor.execute("SELECT name FROM schema_migrations")}
        pending = [(name, action) for name, action in self._get_migrations() if name not in applied]
        if not pending:
            return
        # BEGIN IMMEDIATE serializes migration writers so two bot instances
        # cannot execute the same unapplied migration concurrently. Every
        # migration still pending once the lock is held and its ledger row
        # belongs to this one transaction: COMMIT publishes the whole batch,
        # while any failure reaches the outer ROLLBACK and discards it.
        cursor.execute("BEGIN IMMEDIATE")
        try:
            # Re-read applied inside the lock — another instance may have
            # finished while we were waiting on the busy_timeout.
            applied = {row["name"] for row in cursor.execute("SELECT name FROM schema_migrations")}
            for name, action in pending:
                if name in applied:
                    continue
                logger.info(f"Applying migration: {name}")
                action(cursor)
                cursor.execute(
                    "INSERT INTO schema_migrations (name) VALUES (?)",
                    (name,),
                )
            cursor.execute("COMMIT")
        except Exception:
            try:
                cursor.execute("ROLLBACK")
            except sqlite3.Error as rollback_exc:
                logger.error("Outer migration rollback failed: %s", rollback_exc)
            raise

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
            # Concurrent match support migrations
            ("restructure_pending_matches_for_concurrent", self._migration_restructure_pending_matches_for_concurrent),
            ("add_pending_match_id_to_bets", self._migration_add_pending_match_id_to_bets),
            # Package deal feature
            ("create_package_deals_table", self._migration_create_package_deals_table),
            # Bankruptcy wheel expansion: track normal vs bankrupt spins for CHAIN_REACTION
            ("add_is_bankrupt_to_wheel_spins", self._migration_add_is_bankrupt_to_wheel_spins),
            # Golden wheel: track golden wheel spins separately
            ("add_is_golden_to_wheel_spins", self._migration_add_is_golden_to_wheel_spins),
            # Comeback mechanic: one-use pardon token for next BANKRUPT
            ("add_wheel_pardon_to_players", self._migration_add_wheel_pardon_to_players),
            # Trivia cooldown tracking
            ("add_last_trivia_session_to_players", self._migration_add_last_trivia_session),
            ("create_player_mana_table", self._migration_create_player_mana_table),
            # Trivia session recording for leaderboard
            ("create_trivia_sessions_table", self._migration_create_trivia_sessions_table),
            # Mana shop items and daily loss tracking
            ("create_mana_shop_items_table", self._migration_create_mana_shop_items_table),
            ("create_mana_daily_losses_table", self._migration_create_mana_daily_losses_table),
            ("add_solo_grinder_columns", self._migration_add_solo_grinder_columns),
            ("create_dig_system_tables", self._migration_create_dig_system_tables),
            ("dig_expansion_luminosity_and_buffs", self._migration_dig_expansion),
            ("dig_prestige_events_columns", self._migration_dig_prestige_events),
            ("dig_void_bait_column", self._migration_dig_void_bait),
            ("dig_weather_table", self._migration_dig_weather_table),
            ("dig_thick_skin_date", self._migration_dig_thick_skin_date),
            ("dig_engine_mode_column", self._migration_dig_engine_mode),
            ("dig_personality_table", self._migration_dig_personality_table),
            ("dig_miner_profile_columns", self._migration_dig_miner_profile),
            ("create_dig_boss_echoes", self._migration_create_dig_boss_echoes),
            # Multi-guild lobby isolation
            ("add_guild_id_to_lobby_state", self._migration_add_guild_id_to_lobby_state),
            # Multi-boss tiers + reactive mid-fight prompts (feat/dig-multi-boss-tiers)
            ("create_dig_active_duels", self._migration_create_dig_active_duels),
            ("upgrade_boss_progress_json", self._migration_upgrade_boss_progress_json),
            ("rekey_dig_boss_echoes_by_boss_id", self._migration_rekey_dig_boss_echoes_by_boss_id),
            ("add_stinger_curse_to_tunnels", self._migration_add_stinger_curse_to_tunnels),
            ("clear_active_boss_ids_for_pool_reroll", self._migration_clear_active_boss_ids_for_pool_reroll),
            # Predictions: continuous-quote order-book rework
            ("predictions_orderbook_v1", self._migration_predictions_orderbook),
            ("predictions_prev_price_v1", self._migration_predictions_prev_price),
            ("prediction_trades_last_fill_price", self._migration_prediction_trades_last_fill_price),
            # Dig boss-gear: persistent equipment with durability + relic equip wiring
            ("create_dig_gear_system", self._migration_create_dig_gear_system),
            # Persist betting_mode per match so /admin correctmatch can reverse
            # payouts using the same formula they were settled with.
            ("add_betting_mode_to_matches", self._migration_add_betting_mode_to_matches),
            # Persist actual JC awarded per participant so the balance-history
            # chart can't drift when reward constants or penalty rules change.
            ("add_bonus_jc_to_match_participants", self._migration_add_bonus_jc_to_match_participants),
            # Boss revamp: scaling rebalance, persisted boss HP, pinnacle boss,
            # luminosity-as-real-resource, retreat cost, dialogue v2.
            ("dig_boss_revamp_columns", self._migration_dig_boss_revamp_columns),
            ("dig_boss_progress_persistent_hp", self._migration_dig_boss_progress_persistent_hp),
            # 10:1 prediction-market stock split + EOD-fair history table +
            # one-shot digest banner sentinel.
            ("predictions_mini_split_v1", self._migration_predictions_mini_split_v1),
            # Backfill prediction_fair_snapshots from prediction_levels.posted_at
            # so charts for pre-migration markets aren't a single flat point.
            (
                "predictions_fair_history_backfill_from_levels",
                self._migration_predictions_fair_history_backfill_from_levels,
            ),
            # Cheer cooldown decoupled from free-dig cooldown.
            ("add_last_cheer_at_to_tunnels", self._migration_add_last_cheer_at_to_tunnels),
            ("create_reminder_preferences_table", self._migration_create_reminder_preferences_table),
            ("add_dig_enabled_to_reminder_preferences", self._migration_add_dig_enabled_to_reminder_preferences),
            # Drop the unused mana shop / mana daily loss tables. The
            # delayed-token paths that wrote to them never had readers.
            ("drop_mana_shop_items_table", self._migration_drop_mana_shop_items_table),
            ("drop_mana_daily_losses_table", self._migration_drop_mana_daily_losses_table),
            (
                "renumber_pickaxe_tier_for_stormrend_insert",
                self._migration_renumber_pickaxe_tier_for_stormrend_insert,
            ),
            ("create_dig_dm_memory_table", self._migration_create_dig_dm_memory_table),
            (
                "clear_dig_active_duels_for_retired_timed_mechanics",
                self._migration_clear_dig_active_duels_for_retired_timed_mechanics,
            ),
            # Daily mana flags for bankruptcy-specific buffs (Green insurance, Red re-roll)
            ("add_bankrupt_buff_flags_to_player_mana", self._migration_add_bankrupt_buff_flags_to_player_mana),
            # Witch's Curse: per-target hex with anonymous casters and 7-day duration
            ("create_curses_table", self._migration_create_curses_table),
            # Guild-wide modifiers (e.g. dig "bell" effects that bias all
            # diggers in a guild for a short window).
            ("create_dig_guild_modifiers_table", self._migration_create_dig_guild_modifiers_table),
            # Per-(player, guild) Dota daily-play streak. Adds two columns and
            # backfills consecutive prior play-days from match history.
            ("add_dota_streak_to_players", self._migration_add_dota_streak_to_players),
            # Manashop rework: consumed-today tap flag, per-item daily use tracking,
            # 24h buff store, slow-drip idle-income claims.
            ("add_consumed_today_to_player_mana", self._migration_add_consumed_today_to_player_mana),
            ("create_manashop_daily_uses_table", self._migration_create_manashop_daily_uses_table),
            ("create_manashop_buffs_table", self._migration_create_manashop_buffs_table),
            ("create_slow_drip_claims_table", self._migration_create_slow_drip_claims_table),
            # Index for the dig leaderboard surface. ORDER BY prestige DESC,
            # depth DESC scans guild_id-filtered rows; the composite covers it.
            ("add_tunnels_leaderboard_index", self._migration_add_tunnels_leaderboard_index),
            ("create_dig_quests_table", self._migration_create_dig_quests_table),
            # Multi-charge Grappling Hook + pending Sonar Pulse skip flag.
            ("dig_buff_fun_charges", self._migration_dig_buff_fun_charges),
            # Track shop protect-hero purchases against pending games so
            # profile stats can report the protected-hero win rate.
            (
                "create_protected_hero_purchases_table",
                self._migration_create_protected_hero_purchases_table,
            ),
            # Drop the obsolete pari-mutuel prediction table. The order-book
            # rework replaced it and the last readers have been removed.
            ("drop_prediction_bets_table", self._migration_drop_prediction_bets_table),
            # Event curses: per-tunnel lingering hex from a failed risky event
            # choice (the dig "curse" threat). Mirrors the temp_buffs column.
            ("add_temp_curses_to_tunnels", self._migration_add_temp_curses_to_tunnels),
            # Persist the amulet's crit_chance / crit_bonus in paused boss
            # duels so resumed fights keep the gear-derived crit instead of
            # falling back to the bare risk-tier baseline.
            (
                "add_amulet_crit_to_dig_active_duels",
                self._migration_add_amulet_crit_to_dig_active_duels,
            ),
            # Preferred Dota server region (explicit pick) + inferred fallback
            # from OpenDota play counts. See utils/region.
            ("add_region_columns_to_players", self._migration_add_region_columns),
            # Cap equipped relics at the new ceiling: add the cave-in-free streak
            # counter + a one-time trim-notice flag, then unequip each player's
            # over-cap relics (keeping their 6 newest) and flag them for the notice.
            ("relic_loadout_cap_and_streak", self._migration_relic_loadout_cap_and_streak),
            # Retroactively grant post-prestige S-stat points for bosses that
            # were fully defeated while legacy global boss-award ledgers still
            # blocked re-clears from paying.
            (
                "reconcile_post_prestige_boss_stat_points",
                self._migration_reconcile_post_prestige_boss_stat_points,
            ),
            # Completed prestige levels imply prior full clears. The earlier
            # reconciliation only repaired the current run's boss_progress.
            (
                "reconcile_cumulative_prestige_boss_stat_points",
                self._migration_reconcile_cumulative_prestige_boss_stat_points,
            ),
            # Coalesce legacy NULL guild_id rows (pre-normalize_guild_id writes).
            (
                "normalize_null_guild_id_pairings_and_neon",
                self._migration_normalize_null_guild_id_pairings_and_neon,
            ),
            # Repair tunnels created after the original gear migration, before
            # create_tunnel started creating an equipped starter weapon row.
            (
                "backfill_missing_dig_weapon_gear",
                self._migration_backfill_missing_dig_weapon_gear,
            ),
            # Legacy Overgrowth rows predating charge-based digs.
            (
                "backfill_overgrowth_charges_remaining",
                self._migration_backfill_overgrowth_charges_remaining,
            ),
            # Per-miner opt-in auto-buy settings for common dig consumables.
            ("add_dig_auto_buy_settings", self._migration_add_dig_auto_buy_settings),
            # Stable identity for event-only horizontal gear side-grades.
            ("add_item_id_to_dig_gear", self._migration_add_item_id_to_dig_gear),
            # Central economy ledger.
            ("create_economy_ledger_tables", self._migration_create_economy_ledger_tables),
            (
                "backfill_economy_ledger_opening_balances",
                self._migration_backfill_economy_ledger_opening_balances,
            ),
            # Tax Man enforcement cooldowns.
            ("create_tax_fine_cooldowns_table", self._migration_create_tax_fine_cooldowns_table),
            # Daily Mafia subgame
            ("create_mafia_tables", self._migration_create_mafia_tables),
            # Entry-fee economy: persist fees so payout pools can be audited.
            ("add_mafia_entry_fee_column", self._migration_add_mafia_entry_fee_column),
            # Week-long redesign: multi-cycle game state, per-night actions,
            # town bounties, and a per-guild hall-of-fame pointer.
            ("add_mafia_weekly_game_columns", self._migration_add_mafia_weekly_game_columns),
            ("rebuild_mafia_actions_per_night", self._migration_rebuild_mafia_actions_per_night),
            ("create_mafia_bounties_table", self._migration_create_mafia_bounties_table),
            ("create_mafia_meta_table", self._migration_create_mafia_meta_table),
            ("create_mafia_signups_table", self._migration_create_mafia_signups_table),
            # Continuous cadence: games start back-to-back, so more than one can
            # share a calendar start-date — drop the per-date unique constraint.
            ("rebuild_mafia_games_drop_date_unique", self._migration_rebuild_mafia_games_drop_date_unique),
            (
                "create_mafia_phase_reminders_table",
                self._migration_create_mafia_phase_reminders_table,
            ),
            # Persist the bankruptcy penalty withheld at order-book settlement so
            # realized-P&L stats / balance-chart deltas match the JC actually
            # credited (mirrors match_participants.bonus_jc).
            ("add_bankruptcy_penalty_to_prediction_positions", self._migration_add_bankruptcy_penalty_to_prediction_positions),
            # Immutable package-deal purchase log so year-in-review counts deals
            # even after they are consumed/deleted.
            ("create_package_deal_purchases_table", self._migration_create_package_deal_purchases_table),
            # Idempotency key for match recording: stamp the originating pending
            # match on the matches row so a retry after a post-core failure
            # (bet settlement / loan repayment raising) can't double-record.
            ("add_pending_match_id_to_matches", self._migration_add_pending_match_id_to_matches),
            # White-mana hostile-loss protection and its auditable event stream.
            (
                "add_white_shield_remaining_to_player_mana",
                self._migration_add_white_shield_remaining_to_player_mana,
            ),
            (
                "create_mana_protection_tables",
                self._migration_create_mana_protection_tables,
            ),
            ("add_next_match_pot_to_nonprofit_fund", self._migration_add_next_match_pot_to_nonprofit_fund),
            # Remove storage left behind by the retired Wheel War feature.
            ("drop_retired_wheel_war_tables", self._migration_drop_retired_wheel_war_tables),
            # Persistent per-guild cooldown for the paid /shop pingedash command.
            ("add_last_pingedash_to_players", self._migration_add_last_pingedash_to_players),
            # Player-history trivia: immutable question snapshots and atomic
            # per-session scoring/reward state.
            (
                "create_player_trivia_tables",
                self._migration_create_player_trivia_tables,
            ),
            # Persist semantic wheel outcomes so trivia does not have to infer
            # them from balance deltas or display labels.
            (
                "add_player_trivia_wheel_audit_columns",
                self._migration_add_player_trivia_wheel_audit_columns,
            ),
            # Durable copy of finalized nonprofit votes. The active vote table
            # is cleared between proposals and is therefore not historical.
            (
                "create_disburse_vote_history",
                self._migration_create_disburse_vote_history,
            ),
            ("create_duel_challenges_table", self._migration_create_duel_challenges_table),
            (
                "create_economy_policy_tables",
                self._migration_create_economy_policy_tables,
            ),
            ("cap_soft_avoid_games_remaining", self._migration_cap_soft_avoid_games_remaining),
            # Track whether a daily economy event was announced so a failed
            # announcement is retried on the next wake instead of lost.
            (
                "add_announced_at_to_economy_daily_events",
                self._migration_add_announced_at_to_economy_daily_events,
            ),
            # Per-match idempotency claim for the post-core bonus credits
            # (participation / win / streak) so a retry after a post-core
            # failure can't re-pay them.
            ("add_bonuses_paid_to_matches", self._migration_add_bonuses_paid_to_matches),
            # Persist the win-bonus balance delta per winner so a match
            # correction can reverse exactly what the old winners received.
            (
                "add_win_bonus_jc_to_match_participants",
                self._migration_add_win_bonus_jc_to_match_participants,
            ),
            # Stamp the last game-date a daily tunnel bonus applied so the
            # write commits together with the bonus and can't re-apply on a
            # retried dig.
            (
                "add_lantern_stub_date_to_tunnels",
                self._migration_add_lantern_stub_date_to_tunnels,
            ),
            # Accepted duels used to null next_reminder_at; daily unresolved
            # reminders reuse it, so re-arm rows accepted before the change.
            (
                "schedule_unresolved_duel_reminders",
                self._migration_schedule_unresolved_duel_reminders,
            ),
            # Remove storage left behind by the retired Protect Hero shop item.
            (
                "drop_protected_hero_purchases_table",
                self._migration_drop_protected_hero_purchases_table,
            ),
            # Metadata-only audit trail for each actual LLM provider attempt.
            # Prompt/response content and credentials are intentionally excluded.
            (
                "create_llm_request_attempts_table",
                self._migration_create_llm_request_attempts_table,
            ),
            # User-scoped prediction views should not walk every market before
            # probing the market-first position primary key.
            (
                "add_prediction_positions_user_index",
                self._migration_add_prediction_positions_user_index,
            ),
            (
                "add_bets_recent_loss_index",
                self._migration_add_bets_recent_loss_index,
            ),
            (
                "add_dig_action_history_indexes",
                self._migration_add_dig_action_history_indexes,
            ),
            (
                "add_tip_guild_lookup_indexes",
                self._migration_add_tip_guild_lookup_indexes,
            ),
            (
                "add_rating_history_chronology_indexes",
                self._migration_add_rating_history_chronology_indexes,
            ),
            # Persist the current or pending route through each Dig layer.
            (
                "add_route_state_to_tunnels",
                self._migration_add_route_state_to_tunnels,
            ),
        ]

    # --- Migrations ---

    def _migration_create_llm_request_attempts_table(self, cursor) -> None:
        """Create the metadata-only LLM request telemetry table."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS llm_request_attempts (
                attempt_id       INTEGER PRIMARY KEY AUTOINCREMENT,
                feature          TEXT NOT NULL CHECK(length(trim(feature)) > 0),
                operation        TEXT NOT NULL CHECK(length(trim(operation)) > 0),
                provider         TEXT NOT NULL CHECK(length(trim(provider)) > 0),
                model            TEXT NOT NULL CHECK(length(trim(model)) > 0),
                success          INTEGER NOT NULL CHECK(success IN (0, 1)),
                latency_ms       INTEGER NOT NULL CHECK(latency_ms >= 0),
                prompt_tokens    INTEGER CHECK(prompt_tokens >= 0),
                completion_tokens INTEGER CHECK(completion_tokens >= 0),
                total_tokens     INTEGER CHECK(total_tokens >= 0),
                error_type       TEXT,
                created_at       INTEGER NOT NULL
                    DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_llm_attempts_created
            ON llm_request_attempts(created_at, attempt_id)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_llm_attempts_workload
            ON llm_request_attempts(
                feature, operation, provider, model, created_at DESC
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_llm_attempts_provider_model
            ON llm_request_attempts(provider, model, created_at DESC)
            """
        )

    def _migration_add_prediction_positions_user_index(self, cursor) -> None:
        """Index prediction positions for user-scoped lookups."""
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_prediction_positions_user "
            "ON prediction_positions(discord_id, prediction_id)"
        )

    def _migration_add_bets_recent_loss_index(self, cursor) -> None:
        """Cover user/timestamp filters used by recent-loss shop effects."""
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_guild_discord_time "
            "ON bets(guild_id, discord_id, bet_time)"
        )

    def _migration_add_dig_action_history_indexes(self, cursor) -> None:
        """Cover player-scoped dig histories in chronological order."""
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_actions_guild_actor_created
            ON dig_actions(guild_id, actor_id, created_at DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_actions_guild_target_created
            ON dig_actions(guild_id, target_id, created_at DESC)
            """
        )

        # The chronological indexes cover every lookup supported by the old
        # two-column prefixes, so retaining both sets would only make each dig
        # action pay for two redundant index writes.
        cursor.execute("DROP INDEX IF EXISTS idx_dig_actions_guild_actor")
        cursor.execute("DROP INDEX IF EXISTS idx_dig_actions_guild_target")

    def _migration_add_tip_guild_lookup_indexes(self, cursor) -> None:
        """Cover guild-scoped tip histories and aggregate scans."""
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_tip_transactions_guild_sender_time
            ON tip_transactions(guild_id, sender_id, timestamp DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_tip_transactions_guild_recipient_time
            ON tip_transactions(guild_id, recipient_id, timestamp DESC)
            """
        )

    def _migration_add_rating_history_chronology_indexes(self, cursor) -> None:
        """Cover recent guild and player rating-history reads."""
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_rating_history_guild_time
            ON rating_history(guild_id, timestamp DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_rating_history_guild_player_time
            ON rating_history(guild_id, discord_id, timestamp DESC)
            """
        )

    def _migration_add_route_state_to_tunnels(self, cursor) -> None:
        """Persist the active or pending layer route offer."""
        self._add_column_if_not_exists(cursor, "tunnels", "route_state", "TEXT")

    def _migration_add_bonuses_paid_to_matches(self, cursor) -> None:
        """Idempotency claim for the post-core bonus credits.

        The pending_match_id guard makes the atomic core no-op on retry, but
        the post-core bonus credits (participation, win bonus, streak bonus)
        re-ran unguarded, double-paying them whenever a later post-core step
        (e.g. loan repayment) raised and the user retried. The recording path
        claims this flag with a conditional UPDATE before paying; a retry that
        fails to claim skips the credits."""
        self._add_column_if_not_exists(
            cursor, "matches", "bonuses_paid", "INTEGER NOT NULL DEFAULT 0"
        )

    def _migration_schedule_unresolved_duel_reminders(self, cursor) -> None:
        cursor.execute(
            """
            UPDATE duel_challenges
            SET next_reminder_at = CAST(strftime('%s', 'now') AS INTEGER) + 86400
            WHERE status = 'accepted' AND next_reminder_at IS NULL
            """
        )

    def _migration_add_lantern_stub_date_to_tunnels(self, cursor) -> None:
        """Track the last game-date the lantern-stub daily restore applied.

        The restore previously keyed off last_dig_at alone; a dig that failed
        after the restore write left the gate open, so a retry re-applied the
        bonus. The date stamp commits in the same UPDATE as the restore,
        making a re-run within the day a no-op."""
        self._add_column_if_not_exists(
            cursor, "tunnels", "lantern_stub_date", "TEXT"
        )

    def _migration_add_win_bonus_jc_to_match_participants(self, cursor) -> None:
        """Snapshot the win-bonus balance delta (gross minus bankruptcy
        penalty / skims — garnishment is a bookkeeping split that stays in
        the balance) per winner. bonus_jc aggregates every bonus for the
        match, so a correction couldn't isolate the win bonus to reverse it;
        this column records exactly the JC the win bonus put on the balance."""
        self._add_column_if_not_exists(
            cursor, "match_participants", "win_bonus_jc", "INTEGER"
        )

    def _migration_add_announced_at_to_economy_daily_events(self, cursor) -> None:
        self._add_column_if_not_exists(
            cursor, "economy_daily_events", "announced_at", "INTEGER"
        )
        # Pre-migration rows predate announcement tracking; treat them as
        # announced so the wake loop does not re-announce stale events.
        cursor.execute(
            """
            UPDATE economy_daily_events
            SET announced_at = created_at
            WHERE announced_at IS NULL
            """
        )

    def _migration_add_next_match_pot_to_nonprofit_fund(self, cursor) -> None:
        self._add_column_if_not_exists(
            cursor, "nonprofit_fund", "next_match_pot", "INTEGER NOT NULL DEFAULT 0"
        )

    def _migration_drop_retired_wheel_war_tables(self, cursor) -> None:
        cursor.execute("DROP TABLE IF EXISTS war_bets")
        cursor.execute("DROP TABLE IF EXISTS wheel_wars")

    def _migration_drop_protected_hero_purchases_table(self, cursor) -> None:
        cursor.execute("DROP TABLE IF EXISTS protected_hero_purchases")

    def _migration_add_last_pingedash_to_players(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "last_pingedash", "INTEGER")

    def _migration_create_player_trivia_tables(self, cursor) -> None:
        """Create durable player-trivia sessions and question snapshots."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_trivia_sessions (
                session_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                discord_id INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                status TEXT NOT NULL DEFAULT 'active'
                    CHECK(status IN ('active', 'completed', 'timed_out', 'error', 'cancelled')),
                question_count INTEGER NOT NULL DEFAULT 0 CHECK(question_count >= 0),
                score INTEGER NOT NULL DEFAULT 0 CHECK(score >= 0),
                jc_earned INTEGER NOT NULL DEFAULT 0 CHECK(jc_earned >= 0)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_player_trivia_sessions_guild_user_start
            ON player_trivia_sessions(guild_id, discord_id, started_at DESC, session_id DESC)
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_trivia_questions (
                session_id INTEGER NOT NULL,
                question_number INTEGER NOT NULL CHECK(question_number > 0),
                question_key TEXT NOT NULL,
                category TEXT NOT NULL,
                spicy INTEGER NOT NULL DEFAULT 0 CHECK(spicy IN (0, 1)),
                question_text TEXT NOT NULL,
                options_json TEXT NOT NULL,
                correct_index INTEGER NOT NULL CHECK(correct_index >= 0),
                explanation TEXT,
                selected_index INTEGER CHECK(selected_index >= 0),
                is_correct INTEGER CHECK(is_correct IN (0, 1)),
                reward INTEGER NOT NULL DEFAULT 0 CHECK(reward >= 0),
                answered_at INTEGER,
                PRIMARY KEY (session_id, question_number),
                FOREIGN KEY (session_id)
                    REFERENCES player_trivia_sessions(session_id) ON DELETE CASCADE
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_player_trivia_questions_key
            ON player_trivia_questions(question_key, session_id)
            """
        )

    def _migration_add_player_trivia_wheel_audit_columns(self, cursor) -> None:
        """Add stable semantic identifiers and metadata to wheel history."""
        self._add_column_if_not_exists(cursor, "wheel_spins", "outcome_code", "TEXT")
        self._add_column_if_not_exists(
            cursor, "wheel_spins", "is_bonus", "INTEGER NOT NULL DEFAULT 0"
        )
        self._add_column_if_not_exists(cursor, "wheel_spins", "event_id", "TEXT")
        self._add_column_if_not_exists(cursor, "wheel_spins", "outcome_metadata", "TEXT")
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_wheel_spins_guild_player_time
            ON wheel_spins(guild_id, discord_id, spin_time DESC, spin_id DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_wheel_spins_guild_event
            ON wheel_spins(guild_id, event_id)
            """
        )

    def _migration_create_disburse_vote_history(self, cursor) -> None:
        """Create an immutable, guild-scoped nonprofit vote history."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS disburse_vote_history (
                guild_id INTEGER NOT NULL DEFAULT 0,
                proposal_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                vote_method TEXT NOT NULL,
                voted_at INTEGER NOT NULL,
                proposal_outcome TEXT,
                finalized_at INTEGER,
                PRIMARY KEY (guild_id, proposal_id, discord_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_disburse_vote_history_voter
            ON disburse_vote_history(guild_id, discord_id, voted_at DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_disburse_vote_history_proposal
            ON disburse_vote_history(guild_id, proposal_id, finalized_at)
            """
        )

    def _migration_create_duel_challenges_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS duel_challenges (
                challenge_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                message_id INTEGER,
                challenger_id INTEGER NOT NULL,
                recipient_id INTEGER NOT NULL,
                wager INTEGER NOT NULL CHECK (wager BETWEEN 500 AND 1000),
                issuance_fee INTEGER NOT NULL DEFAULT 50 CHECK (issuance_fee = 50),
                status TEXT NOT NULL CHECK (status IN (
                    'pending', 'accepted', 'declined', 'expired',
                    'resolved', 'voided', 'delivery_failed'
                )),
                trial_type TEXT CHECK (
                    trial_type IS NULL OR trial_type IN ('trial_by_combat', 'trial_of_five')
                ),
                challenger_glicko REAL NOT NULL,
                challenger_rd REAL NOT NULL,
                recipient_glicko REAL NOT NULL,
                recipient_rd REAL NOT NULL,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                next_reminder_at INTEGER,
                responded_at INTEGER,
                resolved_at INTEGER,
                winner_id INTEGER,
                resolution_actor_id INTEGER,
                CHECK (challenger_id != recipient_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_duel_guild_status "
            "ON duel_challenges(guild_id, status, created_at DESC)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_duel_challenger_history "
            "ON duel_challenges(guild_id, challenger_id, created_at DESC)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_duel_recipient_history "
            "ON duel_challenges(guild_id, recipient_id, created_at DESC)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_duel_due_expiry "
            "ON duel_challenges(status, expires_at)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_duel_due_reminder "
            "ON duel_challenges(status, next_reminder_at)"
        )

    def _migration_create_economy_policy_tables(self, cursor) -> None:
        """Persist monetary-policy state, daily snapshots, and event cards."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS economy_policy_state (
                guild_id INTEGER PRIMARY KEY,
                mode TEXT NOT NULL DEFAULT 'recovery'
                    CHECK(mode IN ('recovery', 'normal', 'disabled')),
                target_annual_rate REAL NOT NULL DEFAULT -0.035,
                inflation_ceiling REAL NOT NULL DEFAULT 0.02,
                recovery_started_at INTEGER,
                stable_since INTEGER,
                updated_at INTEGER NOT NULL
            )
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS economy_daily_snapshots (
                guild_id INTEGER NOT NULL,
                snapshot_date TEXT NOT NULL,
                captured_at INTEGER NOT NULL,
                player_wallets INTEGER NOT NULL,
                positive_wallets INTEGER NOT NULL,
                visible_debt INTEGER NOT NULL,
                player_count INTEGER NOT NULL,
                average_wallet REAL NOT NULL,
                reserve_available INTEGER NOT NULL,
                reserve_locked INTEGER NOT NULL,
                reserve_next_match_pot INTEGER NOT NULL,
                prediction_open_cash INTEGER NOT NULL,
                wager_escrow INTEGER NOT NULL,
                monetary_stock INTEGER NOT NULL,
                annualized_30d REAL,
                annualized_90d REAL,
                PRIMARY KEY (guild_id, snapshot_date)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_economy_snapshots_guild_date
            ON economy_daily_snapshots(guild_id, snapshot_date DESC)
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS economy_daily_events (
                event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                event_date TEXT NOT NULL,
                name TEXT NOT NULL,
                hero TEXT NOT NULL,
                direction TEXT NOT NULL
                    CHECK(direction IN ('deflationary', 'neutral', 'boon')),
                severity INTEGER NOT NULL CHECK(severity BETWEEN 1 AND 3),
                target_effect_jc INTEGER NOT NULL,
                forecast_flow_jc INTEGER NOT NULL,
                expected_effect_jc INTEGER NOT NULL,
                direct_effect_jc INTEGER NOT NULL DEFAULT 0,
                actual_stock_change_jc INTEGER,
                monetary_stock_before INTEGER NOT NULL,
                effects TEXT NOT NULL,
                announcement TEXT NOT NULL,
                starts_at INTEGER NOT NULL,
                ends_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE(guild_id, event_date)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_economy_events_active
            ON economy_daily_events(guild_id, starts_at, ends_at)
            """
        )

    def _migration_add_glicko_columns(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "glicko_rating", "REAL")
        self._add_column_if_not_exists(cursor, "players", "glicko_rd", "REAL")
        self._add_column_if_not_exists(cursor, "players", "glicko_volatility", "REAL")

    def _migration_add_region_columns(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "preferred_region", "TEXT")
        self._add_column_if_not_exists(cursor, "players", "inferred_region", "TEXT")

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
        # Fresh-install shape matches the final schema after every later
        # migration has been applied: (lobby_id, guild_id) composite PK plus
        # the full column set. Subsequent ALTER-column migrations
        # (``add_lobby_message_columns``, ``add_guild_id_to_lobby_state``,
        # etc.) are guarded by ``_add_column_if_not_exists`` or PRAGMA checks
        # so replaying them on a fresh DB is a no-op.
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS lobby_state (
                lobby_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                players TEXT,
                conditional_players TEXT DEFAULT '[]',
                status TEXT,
                created_by INTEGER,
                created_at TEXT,
                message_id INTEGER,
                channel_id INTEGER,
                thread_id INTEGER,
                embed_message_id INTEGER,
                origin_channel_id INTEGER,
                player_join_times TEXT DEFAULT '{}',
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (lobby_id, guild_id)
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

        # Disbursement history (for /economy reserve)
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

        # Indexes for efficient queries
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_predictions_guild_status "
            "ON predictions(guild_id, status)"
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
        """Create table for player stake pool in draft mode.

        .. note::
            As of 2026-04, this table has no active reader/writer. The pool
            system it was planned for did not ship. The migration is kept so
            existing databases don't drift; do not delete without also writing
            a drop migration to keep dev and prod in sync.
        """
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
        """Create table for spectator pool bets (parimutuel with player cut).

        .. note::
            As of 2026-04, this table has no active reader/writer. The pool
            system it was planned for did not ship. The migration is kept so
            existing databases don't drift; do not delete without also writing
            a drop migration to keep dev and prod in sync.
        """
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

    def _migration_add_betting_mode_to_matches(self, cursor) -> None:
        """Persist the betting mode per match. Without it, match correction has
        no way to reverse house-mode payouts with the right formula."""
        self._add_column_if_not_exists(
            cursor, "matches", "betting_mode", "TEXT DEFAULT 'pool'"
        )

    def _migration_add_bonus_jc_to_match_participants(self, cursor) -> None:
        """Snapshot the JC actually credited (after garnishment / bankruptcy
        penalty) so the balance-history chart can replay real values instead
        of recomputing from the current config constants."""
        self._add_column_if_not_exists(
            cursor, "match_participants", "bonus_jc", "INTEGER"
        )

    def _migration_add_pending_match_id_to_matches(self, cursor) -> None:
        """Stamp the originating pending match on the recorded matches row and
        enforce one match per pending match.

        record_match_core_atomic commits the match (and its win/loss + rating
        writes) before the post-core money side (bet settlement, loan
        repayment) runs. If a post-core step raised, the pending shuffle was
        never cleared, so a user retry re-ran the atomic core and produced a
        duplicate matches row plus a second win/loss + rating update for every
        player. The column plus a partial unique index make the atomic core
        idempotent: the retry finds the existing row and no-ops."""
        self._add_column_if_not_exists(cursor, "matches", "pending_match_id", "INTEGER")
        cursor.execute(
            """
            CREATE UNIQUE INDEX IF NOT EXISTS uq_matches_guild_pending_match
            ON matches(guild_id, pending_match_id)
            WHERE pending_match_id IS NOT NULL
            """
        )

    def _migration_add_bankruptcy_penalty_to_prediction_positions(self, cursor) -> None:
        """Snapshot the bankruptcy penalty withheld from a penalized winner's
        order-book settlement credit so realized-P&L stats and the balance-chart
        delta report the JC actually credited (gross payout - cost - penalty)
        instead of the gross figure. Mirrors match_participants.bonus_jc."""
        self._add_column_if_not_exists(
            cursor, "prediction_positions", "bankruptcy_penalty", "INTEGER NOT NULL DEFAULT 0"
        )

    def _migration_dig_boss_revamp_columns(self, cursor) -> None:
        """Boss revamp: add tunnel columns for luminosity refill anchor,
        pinnacle state, and retreat cooldown."""
        # Slow on-demand luminosity refill anchor (replaces the daily-snap reset).
        self._add_column_if_not_exists(
            cursor, "tunnels", "last_lum_update_at", "INTEGER"
        )
        # Pinnacle state — locked boss id, current phase (1-3), persisted HP,
        # and last engagement time for regen.
        self._add_column_if_not_exists(
            cursor, "tunnels", "pinnacle_boss_id", "TEXT"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "pinnacle_phase", "INTEGER NOT NULL DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "pinnacle_hp_remaining", "INTEGER"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "pinnacle_last_engaged_at", "INTEGER"
        )
        # Retreat cooldown — when retreats from a boss become free again.
        self._add_column_if_not_exists(
            cursor, "tunnels", "retreat_cooldown_until", "INTEGER"
        )

    def _migration_dig_boss_progress_persistent_hp(self, cursor) -> None:
        """Boss revamp: upgrade boss_progress JSON entries from string status
        (``"active"`` / ``"defeated"`` / ``"phase1_defeated"``) to a structured
        dict so we can carry HP / last_outcome / first_meet_seen across
        encounters.

        Each non-null boss_progress JSON is rewritten in place. Legacy string
        values become ``{"status": <value>}``; entries already in dict shape
        are passed through. The reader code in dig_service still handles
        legacy strings as a safety net.
        """
        import json as _json

        cursor.execute(
            "SELECT discord_id, guild_id, boss_progress FROM tunnels "
            "WHERE boss_progress IS NOT NULL AND boss_progress != ''"
        )
        rows = cursor.fetchall()
        for row in rows:
            try:
                bp = _json.loads(row["boss_progress"])
            except (TypeError, ValueError, _json.JSONDecodeError):
                continue
            if not isinstance(bp, dict):
                continue
            changed = False
            for depth_key, value in list(bp.items()):
                if isinstance(value, str):
                    bp[depth_key] = {"status": value}
                    changed = True
                # Already-dict values: leave alone.
            if changed:
                cursor.execute(
                    "UPDATE tunnels SET boss_progress = ? "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (_json.dumps(bp), row["discord_id"], row["guild_id"]),
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

    def _migration_cap_soft_avoid_games_remaining(self, cursor) -> None:
        """Cap durations accumulated by legacy repeat purchases."""
        cursor.execute("UPDATE soft_avoids SET games_remaining = 10 WHERE games_remaining > 10")

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

    def _migration_normalize_null_guild_id_pairings_and_neon(self, cursor) -> None:
        """Set NULL guild_id to 0 on pairings and neon one-time events."""
        cursor.execute("UPDATE player_pairings SET guild_id = 0 WHERE guild_id IS NULL")
        cursor.execute("UPDATE neon_events SET guild_id = 0 WHERE guild_id IS NULL")

    def _migration_restructure_pending_matches_for_concurrent(self, cursor) -> None:
        """
        Restructure pending_matches table to support concurrent matches per guild.

        Changes PRIMARY KEY from guild_id to auto-increment pending_match_id,
        allowing multiple pending matches per guild simultaneously.
        """
        # Check if we already have the new schema (pending_match_id column exists)
        cursor.execute("PRAGMA table_info(pending_matches)")
        columns = {row[1] for row in cursor.fetchall()}
        if "pending_match_id" in columns:
            # New schema exists - clean up any leftover temp table from partial migration
            cursor.execute("DROP TABLE IF EXISTS pending_matches_old")
            return  # Already migrated

        # Check if pending_matches_old exists from a previous failed migration
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='pending_matches_old'")
        old_table_exists = cursor.fetchone() is not None
        if old_table_exists:
            # Previous migration failed - the old table still has the data
            # Drop the incomplete new table if it exists and restore from old
            cursor.execute("DROP TABLE IF EXISTS pending_matches")
        else:
            # Normal case - rename current table to old
            cursor.execute("ALTER TABLE pending_matches RENAME TO pending_matches_old")

        # Create new table with auto-increment ID
        cursor.execute(
            """
            CREATE TABLE pending_matches (
                pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                payload TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_matches_guild ON pending_matches(guild_id)"
        )

        # Migrate existing data (preserve guild_id and payload)
        cursor.execute(
            """
            INSERT INTO pending_matches (guild_id, payload, updated_at)
            SELECT guild_id, payload, updated_at FROM pending_matches_old
            """
        )

        # Drop old table
        cursor.execute("DROP TABLE pending_matches_old")

    def _migration_add_pending_match_id_to_bets(self, cursor) -> None:
        """
        Add pending_match_id column to bets table for concurrent match support.

        This allows bets to be associated with a specific pending match when
        multiple matches are pending simultaneously.
        """
        self._add_column_if_not_exists(cursor, "bets", "pending_match_id", "INTEGER")
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_bets_pending_match ON bets(pending_match_id)"
        )

    def _migration_create_package_deals_table(self, cursor) -> None:
        """Create table for package deal feature (same-team preference)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS package_deals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                buyer_discord_id INTEGER NOT NULL,
                partner_discord_id INTEGER NOT NULL,
                games_remaining INTEGER NOT NULL DEFAULT 10,
                cost_paid INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE (guild_id, buyer_discord_id, partner_discord_id)
            )
            """
        )
        # Index for looking up deals by buyer
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_package_deals_buyer "
            "ON package_deals(guild_id, buyer_discord_id)"
        )
        # Index for looking up deals targeting a player
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_package_deals_partner "
            "ON package_deals(guild_id, partner_discord_id)"
        )
        # Index for efficient active deal lookup
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_package_deals_guild_active "
            "ON package_deals(guild_id, games_remaining)"
        )

    def _migration_create_package_deal_purchases_table(self, cursor) -> None:
        """Create an immutable purchase log for package deals.

        The active ``package_deals`` rows are mutated (games_remaining
        decremented) and DELETEd once consumed, so year-in-review stats that
        read them silently undercount the year. This append-only log records
        every purchase at creation time and survives consumption/deletion.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS package_deal_purchases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL DEFAULT 0,
                buyer_discord_id INTEGER NOT NULL,
                partner_discord_id INTEGER NOT NULL,
                jc_spent INTEGER NOT NULL,
                games_committed INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )
            """
        )
        # Index for looking up a player's purchases (buyer or partner) by time.
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_package_deal_purchases_buyer "
            "ON package_deal_purchases(guild_id, buyer_discord_id, created_at)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_package_deal_purchases_partner "
            "ON package_deal_purchases(guild_id, partner_discord_id, created_at)"
        )

    def _migration_add_is_bankrupt_to_wheel_spins(self, cursor) -> None:
        """Add is_bankrupt column to wheel_spins for CHAIN_REACTION filtering."""
        self._add_column_if_not_exists(cursor, "wheel_spins", "is_bankrupt", "INTEGER DEFAULT 0")

    def _migration_add_is_golden_to_wheel_spins(self, cursor) -> None:
        """Add is_golden column to wheel_spins for golden wheel tracking."""
        self._add_column_if_not_exists(cursor, "wheel_spins", "is_golden", "INTEGER DEFAULT 0")

    def _migration_add_wheel_pardon_to_players(self, cursor) -> None:
        """Add has_wheel_pardon column to players for COMEBACK mechanic one-use pardon token."""
        self._add_column_if_not_exists(cursor, "players", "has_wheel_pardon", "INTEGER DEFAULT 0")

    def _migration_add_last_trivia_session(self, cursor) -> None:
        self._add_column_if_not_exists(cursor, "players", "last_trivia_session", "INTEGER")

    def _migration_create_player_mana_table(self, cursor) -> None:
        """Create table for daily MTG mana land assignments (one row per player per guild)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS player_mana (
                discord_id   INTEGER NOT NULL,
                guild_id     INTEGER NOT NULL DEFAULT 0,
                current_land TEXT,
                assigned_date TEXT,
                created_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_add_bankrupt_buff_flags_to_player_mana(self, cursor) -> None:
        """Track per-day usage of Green insurance and Red re-roll bankruptcy buffs.

        Both reset naturally with each new mana row (4 AM PST).
        """
        self._add_column_if_not_exists(
            cursor, "player_mana", "bankrupt_insurance_used", "INTEGER DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "player_mana", "bankrupt_reroll_used", "INTEGER DEFAULT 0"
        )

    def _migration_add_consumed_today_to_player_mana(self, cursor) -> None:
        """Tap flag: when a player spends their daily mana on an ultimate
        manashop item, the column flips to 1 and all passive mana effects
        deactivate for the day. Resets when claim_mana_atomic runs at next
        4 AM PST."""
        self._add_column_if_not_exists(
            cursor, "player_mana", "consumed_today", "INTEGER DEFAULT 0"
        )

    def _migration_create_manashop_daily_uses_table(self, cursor) -> None:
        """Track per-item per-day usage for mid-tier manashop items
        (1/day cap independent of the 10s slash-command cooldown)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS manashop_daily_uses (
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                item_id    TEXT NOT NULL,
                used_date  TEXT NOT NULL,
                PRIMARY KEY (discord_id, guild_id, item_id, used_date)
            )
            """
        )

    def _migration_create_manashop_buffs_table(self, cursor) -> None:
        """24h-duration buffs from manashop ultimates (Counterspell, Aegis,
        Overgrowth, Sanctuary, Blood Pact, Dark Bargain debt). The original
        ``mana_shop_items`` table was dropped earlier; this is its successor
        with a focused buff-only schema."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS manashop_buffs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                buff_type  TEXT NOT NULL,
                target_id  INTEGER,
                granted_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                triggered  INTEGER NOT NULL DEFAULT 0,
                data       TEXT
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_manashop_buffs_active
            ON manashop_buffs(guild_id, discord_id, buff_type, triggered, expires_at)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_manashop_buffs_target
            ON manashop_buffs(guild_id, target_id, buff_type, triggered, expires_at)
            """
        )

    def _migration_create_slow_drip_claims_table(self, cursor) -> None:
        """Per-day idle-income claim totals for the Slow Drip relic.

        ``claimed_today`` increments lazily on each /dig invocation. Resets
        per (discord_id, guild_id, claim_date) — old rows pruned at convenience.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS slow_drip_claims (
                discord_id     INTEGER NOT NULL,
                guild_id       INTEGER NOT NULL DEFAULT 0,
                claim_date     TEXT NOT NULL,
                claimed_today  INTEGER NOT NULL DEFAULT 0,
                last_claim_at  INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, claim_date)
            )
            """
        )

    def _migration_add_tunnels_leaderboard_index(self, cursor) -> None:
        """Composite index for the per-guild dig leaderboard.

        ``get_top_tunnels`` and ``get_leaderboard`` both filter by guild_id
        and sort by prestige DESC, depth DESC, discord_id ASC. The PK is
        (discord_id, guild_id), which doesn't help these scans; this index
        covers the filter + the entire sort without a temp b-tree.
        """
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_tunnels_guild_leaderboard "
            "ON tunnels (guild_id, prestige_level DESC, depth DESC, discord_id)"
        )

    def _migration_create_curses_table(self, cursor) -> None:
        """Per-target curses with anonymous casters.

        One row per (caster, target, guild) active curse. Multiple rows allowed
        per target. expires_at is a unix epoch second; queries filter
        `WHERE expires_at > now`. No periodic prune — slow-growth accepted.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS curses (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                target_discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                caster_discord_id INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_curses_target_active
            ON curses(target_discord_id, guild_id, expires_at)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_curses_caster_target
            ON curses(caster_discord_id, target_discord_id, guild_id)
            """
        )

    def _migration_create_dig_guild_modifiers_table(self, cursor) -> None:
        """Guild-wide dig modifiers with expiry.

        One row per (guild, modifier_id). On re-trigger the row is upserted
        and ``expires_at`` extended. Queries filter ``WHERE expires_at > now``;
        expired rows are pruned lazily on access.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_guild_modifiers (
                guild_id     INTEGER NOT NULL,
                modifier_id  TEXT    NOT NULL,
                expires_at   INTEGER NOT NULL,
                payload_json TEXT    NOT NULL DEFAULT '{}',
                created_at   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (guild_id, modifier_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_guild_modifiers_active
            ON dig_guild_modifiers(guild_id, expires_at)
            """
        )

    def _migration_create_dig_quests_table(self, cursor) -> None:
        """Per-(player, guild) quest progression state.

        ``active_quest_id``/``active_quest_step`` hold the in-progress arc
        (NULL when none). ``completed_quests`` is a JSON array of quest ids
        the player has already finished in this guild — completed quests
        never re-fire. State persists across prestige resets.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_quests (
                discord_id        INTEGER NOT NULL,
                guild_id          INTEGER NOT NULL,
                active_quest_id   TEXT,
                active_quest_step INTEGER,
                completed_quests  TEXT NOT NULL DEFAULT '[]',
                last_updated_at   INTEGER,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_add_dota_streak_to_players(self, cursor) -> None:
        """Add Dota daily-play streak fields and backfill from match history.

        ``dota_streak_days`` counts consecutive game-days the player recorded
        ≥1 match (excluded/bench games never appear in match_participants, so
        they're naturally filtered out). ``dota_last_played_date`` is the
        most recent game-date string they played. Both reuse the same 4 AM PST
        rollover that /dig uses, so the two systems can't drift on date math.

        Backfill walks per-(player, guild) match history backward from the
        most recent play-date. Stops on the first gap. Idempotent: re-running
        is a no-op because the migration is recorded in schema_migrations.
        """
        import datetime as _dt

        from utils.game_date import game_date_for, yesterday_of

        self._add_column_if_not_exists(cursor, "players", "dota_streak_days", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "players", "dota_last_played_date", "TEXT")

        rows = cursor.execute(
            """
            SELECT mp.discord_id, mp.guild_id, m.match_date
            FROM match_participants mp
            JOIN matches m ON m.match_id = mp.match_id
            ORDER BY mp.discord_id, mp.guild_id, m.match_date DESC
            """
        ).fetchall()
        if not rows:
            return

        per_player: dict[tuple[int, int], list[str]] = {}
        for row in rows:
            discord_id = row["discord_id"] if hasattr(row, "keys") else row[0]
            guild_id = row["guild_id"] if hasattr(row, "keys") else row[1]
            raw_date = row["match_date"] if hasattr(row, "keys") else row[2]
            if guild_id is None:
                guild_id = 0
            if not raw_date:
                continue
            try:
                if isinstance(raw_date, str):
                    parsed = _dt.datetime.fromisoformat(raw_date.replace(" ", "T"))
                else:
                    parsed = raw_date
            except (TypeError, ValueError):
                continue
            game_date = game_date_for(parsed)
            key = (int(discord_id), int(guild_id))
            dates = per_player.setdefault(key, [])
            # The SELECT above is ORDER BY match_date DESC, so rows arrive
            # newest-first. Skip any same-day repeats by comparing against
            # the most recent entry; the streak walk below also depends on
            # this DESC ordering — don't change one without the other.
            if not dates or dates[-1] != game_date:
                dates.append(game_date)

        for (discord_id, guild_id), dates_desc in per_player.items():
            if not dates_desc:
                continue
            last_played = dates_desc[0]
            streak = 1
            prev = last_played
            for d in dates_desc[1:]:
                if d == yesterday_of(prev):
                    streak += 1
                    prev = d
                else:
                    break
            cursor.execute(
                """
                UPDATE players
                SET dota_streak_days = ?, dota_last_played_date = ?
                WHERE discord_id = ? AND guild_id = ?
                """,
                (streak, last_played, discord_id, guild_id),
            )

    def _migration_create_trivia_sessions_table(self, cursor) -> None:
        """Create table for recording trivia session results (leaderboard)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS trivia_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                streak INTEGER NOT NULL DEFAULT 0,
                jc_earned INTEGER NOT NULL DEFAULT 0,
                played_at INTEGER NOT NULL
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_trivia_sessions_guild_played
            ON trivia_sessions(guild_id, played_at)
            """
        )

    def _migration_create_mana_shop_items_table(self, cursor) -> None:
        """Create table for mana-exclusive shop items (Guardian Angel, Mana Shield, etc.)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mana_shop_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                item_type TEXT NOT NULL,
                target_id INTEGER,
                purchased_at INTEGER NOT NULL,
                expires_at INTEGER,
                triggered INTEGER NOT NULL DEFAULT 0,
                data TEXT
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_mana_shop_items_guild_discord
            ON mana_shop_items(guild_id, discord_id)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_mana_shop_items_type_active
            ON mana_shop_items(guild_id, item_type, triggered)
            """
        )

    def _migration_create_mana_daily_losses_table(self, cursor) -> None:
        """Create table for tracking daily JC losses (for Green's Regrowth shop item)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mana_daily_losses (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                loss_date TEXT NOT NULL,
                total_lost INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id, loss_date)
            )
            """
        )

    def _migration_add_solo_grinder_columns(self, cursor) -> None:
        """Add columns for solo ranked grinder detection."""
        self._add_column_if_not_exists(cursor, "players", "is_solo_grinder", "INTEGER DEFAULT 0")
        self._add_column_if_not_exists(cursor, "players", "solo_grinder_checked_at", "TEXT")

    def _migration_create_dig_system_tables(self, cursor) -> None:
        """Create all tables for the tunnel digging minigame."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS tunnels (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                depth            INTEGER NOT NULL DEFAULT 0,
                max_depth        INTEGER NOT NULL DEFAULT 0,
                total_digs       INTEGER NOT NULL DEFAULT 0,
                total_jc_earned  INTEGER NOT NULL DEFAULT 0,
                last_dig_at      INTEGER,
                streak_days      INTEGER NOT NULL DEFAULT 0,
                streak_last_date TEXT,
                pickaxe_tier     INTEGER NOT NULL DEFAULT 0,
                prestige_level   INTEGER NOT NULL DEFAULT 0,
                prestige_perks   TEXT,
                tunnel_name      TEXT,
                boss_progress    TEXT,
                boss_attempts    TEXT,
                trap_active      INTEGER NOT NULL DEFAULT 0,
                trap_free_today  INTEGER NOT NULL DEFAULT 1,
                trap_date        TEXT,
                insured_until    INTEGER,
                reinforced_until INTEGER,
                injury_state     TEXT,
                paid_digs_today  INTEGER NOT NULL DEFAULT 0,
                paid_dig_date    TEXT,
                revenge_target   INTEGER,
                revenge_type     TEXT,
                revenge_until    INTEGER,
                hard_hat_charges INTEGER NOT NULL DEFAULT 0,
                void_bait_digs   INTEGER NOT NULL DEFAULT 0,
                cheer_data       TEXT,
                created_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_actions (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                actor_id         INTEGER NOT NULL,
                target_id        INTEGER,
                action_type      TEXT NOT NULL,
                depth_before     INTEGER NOT NULL,
                depth_after      INTEGER NOT NULL,
                jc_delta         INTEGER NOT NULL DEFAULT 0,
                detail           TEXT,
                created_at       INTEGER NOT NULL
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_actions_guild_actor
            ON dig_actions(guild_id, actor_id)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_actions_guild_target
            ON dig_actions(guild_id, target_id)
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_inventory (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                item_type        TEXT NOT NULL,
                queued           INTEGER NOT NULL DEFAULT 0,
                created_at       INTEGER NOT NULL
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_inventory_player
            ON dig_inventory(discord_id, guild_id)
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_artifacts (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                artifact_id      TEXT NOT NULL,
                found_at         INTEGER NOT NULL,
                is_relic         INTEGER NOT NULL DEFAULT 0,
                equipped         INTEGER NOT NULL DEFAULT 0
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_artifacts_player
            ON dig_artifacts(discord_id, guild_id)
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_achievements (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                achievement_id   TEXT NOT NULL,
                unlocked_at      INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id, achievement_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_artifact_registry (
                artifact_id      TEXT NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                first_finder_id  INTEGER,
                first_found_at   INTEGER,
                total_found      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (artifact_id, guild_id)
            )
            """
        )

    def _migration_dig_expansion(self, cursor) -> None:
        """Add luminosity and temp buff columns for the dig expansion."""
        self._add_column_if_not_exists(cursor, "tunnels", "luminosity", "INTEGER NOT NULL DEFAULT 100")
        self._add_column_if_not_exists(cursor, "tunnels", "temp_buffs", "TEXT")

    def _migration_dig_prestige_events(self, cursor) -> None:
        """Add prestige run tracking and mutation columns for the dig prestige/events expansion."""
        self._add_column_if_not_exists(cursor, "tunnels", "best_run_score", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "tunnels", "current_run_jc", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "tunnels", "current_run_artifacts", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "tunnels", "current_run_events", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "tunnels", "total_prestige_score", "INTEGER NOT NULL DEFAULT 0")
        self._add_column_if_not_exists(cursor, "tunnels", "mutations", "TEXT")

    def _migration_dig_void_bait(self, cursor) -> None:
        """Add void_bait_digs column for tracking Void Bait charges."""
        self._add_column_if_not_exists(cursor, "tunnels", "void_bait_digs", "INTEGER NOT NULL DEFAULT 0")

    def _migration_dig_weather_table(self, cursor) -> None:
        """Create dig_weather table for daily layer weather conditions."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_weather (
                guild_id    INTEGER NOT NULL,
                game_date   TEXT NOT NULL,
                layer_name  TEXT NOT NULL,
                weather_id  TEXT NOT NULL,
                PRIMARY KEY (guild_id, game_date, layer_name)
            )
            """
        )

    def _migration_dig_thick_skin_date(self, cursor) -> None:
        """Track last date the thick_skin mutation consumed its daily shield.

        Without this column, `DigService._apply_cave_in_mutations` crashes when
        the ``thick_skin`` mutation is active because it calls
        ``update_tunnel(thick_skin_date=today)`` against a non-existent column.
        """
        self._add_column_if_not_exists(cursor, "tunnels", "thick_skin_date", "TEXT")

    def _migration_dig_engine_mode(self, cursor) -> None:
        """Add engine_mode column to tunnels for legacy/llm toggle."""
        self._add_column_if_not_exists(
            cursor, "tunnels", "engine_mode", "TEXT NOT NULL DEFAULT 'legacy'"
        )

    def _migration_dig_personality_table(self, cursor) -> None:
        """Create dig_personality table for LLM player personality tracking."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_personality (
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                summary          TEXT DEFAULT '',
                choice_histogram TEXT DEFAULT '{}',
                notable_moments  TEXT DEFAULT '[]',
                play_style       TEXT DEFAULT 'unknown',
                social_summary   TEXT DEFAULT '',
                updated_at       INTEGER DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_dig_miner_profile(self, cursor) -> None:
        """Add miner profile/stat columns used by DM mode and dig mechanics."""
        self._add_column_if_not_exists(
            cursor, "tunnels", "miner_origin", "TEXT NOT NULL DEFAULT ''"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "miner_about", "TEXT NOT NULL DEFAULT ''"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "stat_strength", "INTEGER NOT NULL DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "stat_smarts", "INTEGER NOT NULL DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "stat_stamina", "INTEGER NOT NULL DEFAULT 0"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "stat_points", "INTEGER NOT NULL DEFAULT 5"
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "stat_boss_awards", "TEXT NOT NULL DEFAULT '[]'"
        )

    def _migration_reconcile_post_prestige_boss_stat_points(self, cursor) -> None:
        """Grant missing current-run S points for post-prestige boss clears.

        Older ``stat_boss_awards`` rows stored a global list of boss depths.
        After prestige, that stale list could block re-cleared bosses from
        granting their +1 S-stat point. This one-shot reconciliation looks at
        the current run's defeated tier bosses and brings the per-prestige
        award ledger back into sync.
        """
        boss_boundaries = (25, 50, 75, 100, 150, 200, 275)
        rows = cursor.execute(
            """
            SELECT discord_id, guild_id, prestige_level, stat_points,
                   stat_boss_awards, boss_progress
            FROM tunnels
            WHERE COALESCE(prestige_level, 0) > 0
            """
        ).fetchall()

        for row in rows:
            discord_id, guild_id = row[0], row[1]
            prestige_level = int(row[2] or 0)
            stat_points = row[3]
            stat_boss_awards = row[4]
            raw_boss_progress = row[5]
            try:
                boss_progress = json.loads(raw_boss_progress or "{}")
            except (json.JSONDecodeError, TypeError):
                continue
            if not isinstance(boss_progress, dict):
                continue

            defeated = set()
            for boundary in boss_boundaries:
                entry = boss_progress.get(str(boundary))
                status = entry.get("status") if isinstance(entry, dict) else entry
                if status == "defeated":
                    defeated.add(boundary)

            if not defeated:
                continue

            awarded = set()
            try:
                decoded_awards = json.loads(stat_boss_awards or "[]")
            except (json.JSONDecodeError, TypeError):
                decoded_awards = []

            if isinstance(decoded_awards, dict):
                try:
                    stored_prestige = int(decoded_awards.get("prestige_level", 0) or 0)
                except (TypeError, ValueError):
                    stored_prestige = 0
                if stored_prestige == prestige_level:
                    award_values = decoded_awards.get("awards", [])
                else:
                    award_values = []
            elif isinstance(decoded_awards, list):
                # Legacy global ledgers are stale for P1+ runs, matching the
                # service read path in ProgressionMixin._get_stat_boss_awards.
                award_values = []
            else:
                award_values = []

            if isinstance(award_values, list):
                for value in award_values:
                    try:
                        awarded.add(int(value))
                    except (TypeError, ValueError):
                        continue

            missing = sorted(defeated - awarded)
            if not missing:
                continue

            expected_points = 5 + (prestige_level * len(boss_boundaries)) + len(defeated)
            current_points = int(stat_points or 5)
            if current_points >= expected_points:
                updated_awards = sorted(awarded | defeated)
                cursor.execute(
                    """
                    UPDATE tunnels
                    SET stat_boss_awards = ?
                    WHERE discord_id = ? AND guild_id = ?
                    """,
                    (
                        json.dumps({
                            "prestige_level": prestige_level,
                            "awards": updated_awards,
                        }),
                        discord_id,
                        guild_id,
                    ),
                )
                continue

            updated_awards = sorted(awarded | defeated)
            updated_stat_points = max(5, current_points) + len(missing)
            cursor.execute(
                """
                UPDATE tunnels
                SET stat_points = ?, stat_boss_awards = ?
                WHERE discord_id = ? AND guild_id = ?
                """,
                (
                    updated_stat_points,
                    json.dumps({
                        "prestige_level": prestige_level,
                        "awards": updated_awards,
                    }),
                    discord_id,
                    guild_id,
                ),
            )

    def _migration_reconcile_cumulative_prestige_boss_stat_points(self, cursor) -> None:
        """Top up S points implied by completed prestige clears.

        ``prestige_level`` is the number of completed ascensions. Each completed
        ascension required all seven tier bosses, and the current run can award
        those seven again. The current-run award ledger intentionally only
        tracks this run, so this migration repairs the cumulative ``stat_points``
        total without trying to encode prior-run awards into that ledger.
        """
        boss_boundaries = (25, 50, 75, 100, 150, 200, 275)
        rows = cursor.execute(
            """
            SELECT discord_id, guild_id, prestige_level, stat_points,
                   stat_boss_awards, boss_progress
            FROM tunnels
            WHERE COALESCE(prestige_level, 0) > 0
            """
        ).fetchall()

        for row in rows:
            discord_id, guild_id = row[0], row[1]
            prestige_level = int(row[2] or 0)
            stat_points = int(row[3] or 5)
            stat_boss_awards = row[4]
            raw_boss_progress = row[5]

            current_defeated = set()
            try:
                boss_progress = json.loads(raw_boss_progress or "{}")
            except (json.JSONDecodeError, TypeError):
                boss_progress = {}
            if isinstance(boss_progress, dict):
                for boundary in boss_boundaries:
                    entry = boss_progress.get(str(boundary))
                    status = entry.get("status") if isinstance(entry, dict) else entry
                    if status == "defeated":
                        current_defeated.add(boundary)

            expected_points = 5 + (prestige_level * len(boss_boundaries))
            expected_points += len(current_defeated)
            if stat_points >= expected_points:
                continue

            awarded = set()
            try:
                decoded_awards = json.loads(stat_boss_awards or "[]")
            except (json.JSONDecodeError, TypeError):
                decoded_awards = []
            if isinstance(decoded_awards, dict):
                try:
                    stored_prestige = int(decoded_awards.get("prestige_level", 0) or 0)
                except (TypeError, ValueError):
                    stored_prestige = 0
                if stored_prestige == prestige_level:
                    award_values = decoded_awards.get("awards", [])
                else:
                    award_values = []
            elif isinstance(decoded_awards, list):
                award_values = []
            else:
                award_values = []

            if isinstance(award_values, list):
                for value in award_values:
                    try:
                        awarded.add(int(value))
                    except (TypeError, ValueError):
                        continue

            updated_awards = sorted(awarded | current_defeated)
            cursor.execute(
                """
                UPDATE tunnels
                SET stat_points = ?, stat_boss_awards = ?
                WHERE discord_id = ? AND guild_id = ?
                """,
                (
                    expected_points,
                    json.dumps({
                        "prestige_level": prestige_level,
                        "awards": updated_awards,
                    }),
                    discord_id,
                    guild_id,
                ),
            )

    def _migration_create_dig_boss_echoes(self, cursor) -> None:
        """Per-guild, per-boss 'echo' window.

        After a guild's first kill of a boss, subsequent fighters at that
        same boundary see the boss weakened for a fixed window. The row is
        upserted on every kill so the window restarts.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_boss_echoes (
                guild_id INTEGER NOT NULL,
                depth INTEGER NOT NULL,
                killer_discord_id INTEGER NOT NULL,
                weakened_until INTEGER NOT NULL,
                PRIMARY KEY (guild_id, depth)
            )
            """
        )

    def _migration_create_dig_active_duels(self, cursor) -> None:
        """Per-player mid-fight duel state for boss fights.

        Rows exist only while a duel is paused awaiting a mid-fight prompt
        response. ``start_boss_duel`` inserts the row when the rolled mechanic
        triggers; ``resume_boss_duel`` reads it, applies the player's choice,
        continues the duel, and deletes the row on final resolution. Survives
        bot restarts so in-flight fights don't drop.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_active_duels (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                boss_id TEXT NOT NULL,
                tier INTEGER NOT NULL,
                mechanic_id TEXT NOT NULL,
                risk_tier TEXT NOT NULL,
                wager INTEGER NOT NULL,
                player_hp INTEGER NOT NULL,
                boss_hp INTEGER NOT NULL,
                round_num INTEGER NOT NULL,
                round_log TEXT NOT NULL DEFAULT '[]',
                pending_prompt TEXT,
                rng_state TEXT NOT NULL,
                status_effects TEXT NOT NULL DEFAULT '{}',
                echo_applied INTEGER NOT NULL DEFAULT 0,
                echo_killer_id INTEGER,
                player_hit REAL NOT NULL,
                player_dmg INTEGER NOT NULL,
                boss_hit REAL NOT NULL,
                boss_dmg INTEGER NOT NULL,
                crit_chance REAL NOT NULL DEFAULT 0,
                crit_bonus INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                last_interaction_at INTEGER NOT NULL,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_add_amulet_crit_to_dig_active_duels(self, cursor) -> None:
        """Add crit_chance / crit_bonus columns to paused boss-duel rows."""
        self._add_column_if_not_exists(
            cursor, "dig_active_duels", "crit_chance", "REAL NOT NULL DEFAULT 0",
        )
        self._add_column_if_not_exists(
            cursor, "dig_active_duels", "crit_bonus", "INTEGER NOT NULL DEFAULT 0",
        )

    def _migration_upgrade_boss_progress_json(self, cursor) -> None:
        """Migrate ``tunnels.boss_progress`` JSON to the new {boss_id, status} shape.

        Old shape:  ``{"25": "active"|"phase1_defeated"|"defeated"}``
        New shape:  ``{"25": {"boss_id": "grothak", "status": "active"}}``

        Backfills each existing depth entry with the grandfathered boss id so
        players who were in the middle of a pre-feature run keep their locked
        boss.
        """
        import json as _json

        legacy_boss_ids = {
            25: "grothak",
            50: "crystalia",
            75: "magmus_rex",
            100: "void_warden",
            150: "sporeling_sovereign",
            200: "chronofrost",
            275: "nameless_depth",
        }

        cursor.execute(
            "SELECT discord_id, guild_id, boss_progress FROM tunnels "
            "WHERE boss_progress IS NOT NULL AND boss_progress != ''"
        )
        for row in cursor.fetchall():
            discord_id = row[0]
            guild_id = row[1]
            raw = row[2]
            try:
                data = _json.loads(raw)
            except Exception:
                continue
            if not isinstance(data, dict):
                continue
            changed = False
            upgraded: dict = {}
            for depth_key, val in data.items():
                if isinstance(val, str):
                    # Legacy shape — upgrade.
                    try:
                        depth_int = int(depth_key)
                    except (TypeError, ValueError):
                        upgraded[depth_key] = val
                        continue
                    upgraded[depth_key] = {
                        "boss_id": legacy_boss_ids.get(depth_int, "") if val != "active" else "",
                        "status": val,
                    }
                    changed = True
                else:
                    upgraded[depth_key] = val
            if changed:
                cursor.execute(
                    "UPDATE tunnels SET boss_progress = ? "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (_json.dumps(upgraded), discord_id, guild_id),
                )

    def _migration_rekey_dig_boss_echoes_by_boss_id(self, cursor) -> None:
        """Re-key ``dig_boss_echoes`` from (guild_id, depth) to (guild_id, boss_id).

        With multiple bosses per tier, killing one boss at a depth should only
        weaken that specific boss for guildmates — not every boss at the tier.
        Backfills existing rows with the grandfathered boss id for that depth.

        Crash-safe against a mid-migration interruption: if a previous run
        RENAMEd the original to ``dig_boss_echoes_old`` but died before
        creating the new table, this method detects ``_old`` still exists
        and resumes from the create step.
        """
        cursor.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "AND name IN ('dig_boss_echoes', 'dig_boss_echoes_old')"
        )
        present = {row[0] for row in cursor.fetchall()}
        has_live = "dig_boss_echoes" in present
        has_old = "dig_boss_echoes_old" in present

        if has_live:
            cursor.execute("PRAGMA table_info(dig_boss_echoes)")
            columns = {row[1] for row in cursor.fetchall()}
            if "boss_id" in columns and not has_old:
                return  # Fully migrated already.

        legacy_boss_ids = {
            25: "grothak",
            50: "crystalia",
            75: "magmus_rex",
            100: "void_warden",
            150: "sporeling_sovereign",
            200: "chronofrost",
            275: "nameless_depth",
        }

        # Normal path: rename live → _old so we can rebuild. Skipped if a
        # prior crashed run already did the rename.
        if has_live and not has_old:
            cursor.execute("ALTER TABLE dig_boss_echoes RENAME TO dig_boss_echoes_old")
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_boss_echoes (
                guild_id INTEGER NOT NULL,
                boss_id TEXT NOT NULL,
                depth INTEGER NOT NULL,
                killer_discord_id INTEGER NOT NULL,
                weakened_until INTEGER NOT NULL,
                PRIMARY KEY (guild_id, boss_id)
            )
            """
        )
        # Copy rows from the legacy table. ``INSERT OR REPLACE`` makes the
        # copy idempotent so a mid-migration retry doesn't double-apply rows.
        cursor.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='dig_boss_echoes_old'"
        )
        if cursor.fetchone() is not None:
            cursor.execute(
                "SELECT guild_id, depth, killer_discord_id, weakened_until "
                "FROM dig_boss_echoes_old"
            )
            for row in cursor.fetchall():
                guild_id = row[0]
                depth = int(row[1])
                killer = row[2]
                weakened = row[3]
                boss_id = legacy_boss_ids.get(depth, f"depth_{depth}")
                cursor.execute(
                    """
                    INSERT OR REPLACE INTO dig_boss_echoes
                        (guild_id, boss_id, depth, killer_discord_id, weakened_until)
                    VALUES (?, ?, ?, ?, ?)
                    """,
                    (guild_id, boss_id, depth, killer, weakened),
                )
            cursor.execute("DROP TABLE dig_boss_echoes_old")

    def _migration_add_stinger_curse_to_tunnels(self, cursor) -> None:
        """Add ``stinger_curse`` JSON column to tunnels for persistent loss debuffs."""
        self._add_column_if_not_exists(cursor, "tunnels", "stinger_curse", "TEXT")

    def _migration_add_temp_curses_to_tunnels(self, cursor) -> None:
        """Add ``temp_curses`` JSON column to tunnels for event-driven hexes.

        Holds a single active curse (the dig "curse" threat) as JSON, shaped
        like a temp buff but with draining effects. Separate from ``temp_buffs``
        so a curse and a buff can be active at the same time.
        """
        self._add_column_if_not_exists(cursor, "tunnels", "temp_curses", "TEXT")

    def _migration_clear_active_boss_ids_for_pool_reroll(self, cursor) -> None:
        """Clear locked boss_id on still-active boss_progress entries.

        The earlier ``upgrade_boss_progress_json`` migration backfilled the
        grandfathered boss_id for every depth — including depths the player
        had not yet engaged. That permanently locked those depths out of the
        multi-boss pool because ``_ensure_boss_locked`` short-circuits when a
        boss_id is already present. This migration clears boss_id on entries
        whose status is still ``"active"``, letting the next encounter roll
        fresh from the tier pool. Defeated and phase1_defeated entries are
        preserved verbatim — they are historical kills and rewriting them
        would falsify stat-points and dig_boss_echoes records.
        """
        import json as _json
        cursor.execute(
            "SELECT discord_id, guild_id, boss_progress FROM tunnels "
            "WHERE boss_progress IS NOT NULL AND boss_progress != ''"
        )
        for row in cursor.fetchall():
            discord_id, guild_id, raw = row[0], row[1], row[2]
            try:
                data = _json.loads(raw)
            except Exception:
                continue
            if not isinstance(data, dict):
                continue
            changed = False
            for val in data.values():
                if (
                    isinstance(val, dict)
                    and val.get("status") == "active"
                    and val.get("boss_id")
                ):
                    val["boss_id"] = ""
                    changed = True
            if changed:
                cursor.execute(
                    "UPDATE tunnels SET boss_progress = ? "
                    "WHERE discord_id = ? AND guild_id = ?",
                    (_json.dumps(data), discord_id, guild_id),
                )

    def _migration_add_guild_id_to_lobby_state(self, cursor) -> None:
        """
        Add guild_id to lobby_state, changing the primary key to (lobby_id, guild_id).

        Lobbies are now per-guild so every guild has its own independent lobby.
        Existing rows are backfilled with guild_id = 0 (normalized None).

        SQLite doesn't support altering primary keys, so we rebuild the table.
        """
        cursor.execute("PRAGMA table_info(lobby_state)")
        columns = {row[1] for row in cursor.fetchall()}
        if "guild_id" in columns:
            return

        cursor.execute(
            """
            CREATE TABLE lobby_state_new (
                lobby_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                players TEXT,
                conditional_players TEXT DEFAULT '[]',
                status TEXT,
                created_by INTEGER,
                created_at TEXT,
                message_id INTEGER,
                channel_id INTEGER,
                thread_id INTEGER,
                embed_message_id INTEGER,
                origin_channel_id INTEGER,
                player_join_times TEXT DEFAULT '{}',
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (lobby_id, guild_id)
            )
            """
        )

        # Copy existing rows; backfill guild_id = 0 for pre-migration lobbies
        # so the legacy single-guild state keeps working.
        cursor.execute(
            """
            INSERT INTO lobby_state_new (
                lobby_id, guild_id, players, conditional_players, status,
                created_by, created_at, message_id, channel_id, thread_id,
                embed_message_id, origin_channel_id, player_join_times, updated_at
            )
            SELECT
                lobby_id,
                0,
                players,
                COALESCE(conditional_players, '[]'),
                status,
                created_by,
                created_at,
                message_id,
                channel_id,
                thread_id,
                embed_message_id,
                origin_channel_id,
                COALESCE(player_join_times, '{}'),
                updated_at
            FROM lobby_state
            """
        )

        cursor.execute("DROP TABLE lobby_state")
        cursor.execute("ALTER TABLE lobby_state_new RENAME TO lobby_state")

    def _migration_predictions_orderbook(self, cursor) -> None:
        """Add columns + tables for the continuous-quote order-book prediction rework.

        - Adds current_price / initial_fair / last_refresh_at / lp_pnl to ``predictions``
        - Creates ``prediction_levels`` (the LP's posted ladder, per market)
        - Creates ``prediction_positions`` (per-user YES/NO holdings + cost basis)
        - Creates ``prediction_trades`` (every fill, for tape display + stats)

        The legacy pari-mutuel ``prediction_bets`` table is dropped by a later
        migration; the ``resolution_votes`` column is reused by the order-book
        resolution-voting flow.
        """
        # New columns on predictions
        self._add_column_if_not_exists(cursor, "predictions", "current_price", "INTEGER")
        self._add_column_if_not_exists(cursor, "predictions", "initial_fair", "INTEGER")
        self._add_column_if_not_exists(cursor, "predictions", "last_refresh_at", "INTEGER")
        self._add_column_if_not_exists(cursor, "predictions", "lp_pnl", "INTEGER DEFAULT 0")

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS prediction_levels (
                level_id INTEGER PRIMARY KEY AUTOINCREMENT,
                prediction_id INTEGER NOT NULL,
                side TEXT NOT NULL,
                price INTEGER NOT NULL,
                remaining_size INTEGER NOT NULL,
                posted_at INTEGER NOT NULL,
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_pred_levels_pred_side_price "
            "ON prediction_levels(prediction_id, side, price)"
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS prediction_positions (
                prediction_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                yes_contracts INTEGER NOT NULL DEFAULT 0,
                yes_cost_basis_total INTEGER NOT NULL DEFAULT 0,
                no_contracts INTEGER NOT NULL DEFAULT 0,
                no_cost_basis_total INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (prediction_id, discord_id),
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS prediction_trades (
                trade_id INTEGER PRIMARY KEY AUTOINCREMENT,
                prediction_id INTEGER NOT NULL,
                discord_id INTEGER NOT NULL,
                action TEXT NOT NULL,
                contracts INTEGER NOT NULL,
                jopacoins INTEGER NOT NULL,
                vwap_x100 INTEGER NOT NULL,
                last_fill_price INTEGER,
                trade_time INTEGER NOT NULL,
                FOREIGN KEY (prediction_id) REFERENCES predictions(prediction_id),
                FOREIGN KEY (discord_id) REFERENCES players(discord_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_pred_trades_pred "
            "ON prediction_trades(prediction_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_pred_trades_user "
            "ON prediction_trades(discord_id)"
        )

    def _migration_predictions_prev_price(self, cursor) -> None:
        """Add prev_price so the daily digest can show a price-change arrow."""
        self._add_column_if_not_exists(cursor, "predictions", "prev_price", "INTEGER")

    def _migration_prediction_trades_last_fill_price(self, cursor) -> None:
        """Record the terminal fill price for through-book refresh anchors."""
        self._add_column_if_not_exists(cursor, "prediction_trades", "last_fill_price", "INTEGER")

    def _migration_create_dig_gear_system(self, cursor) -> None:
        """Per-player persistent boss-combat gear with durability.

        Creates ``dig_gear`` to hold one row per owned piece across the
        Weapon / Armor / Boots slots, plus a partial-unique-index that the
        DB enforces ``one equipped piece per (discord_id, guild_id, slot)``.
        Backfills a Weapon row for every existing tunnel using its current
        ``pickaxe_tier`` so no one loses progression on rollout — the
        legacy ``tunnels.pickaxe_tier`` column is kept in place this
        release as a safety net while service-layer reads migrate over.
        Relics continue to live in ``dig_artifacts``; they are not moved.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_gear (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                discord_id    INTEGER NOT NULL,
                guild_id      INTEGER NOT NULL DEFAULT 0,
                slot          TEXT    NOT NULL,
                tier          INTEGER NOT NULL,
                durability    INTEGER NOT NULL,
                equipped      INTEGER NOT NULL DEFAULT 0,
                acquired_at   INTEGER NOT NULL,
                source        TEXT    NOT NULL DEFAULT 'shop'
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_dig_gear_player_slot
            ON dig_gear(discord_id, guild_id, slot)
            """
        )
        cursor.execute(
            """
            CREATE UNIQUE INDEX IF NOT EXISTS uq_dig_gear_one_equipped_per_slot
            ON dig_gear(discord_id, guild_id, slot) WHERE equipped = 1
            """
        )
        # Backfill the Weapon slot from the legacy pickaxe_tier so existing
        # players don't suddenly fight bosses naked. New artifacts (armor,
        # boots) start empty per spec.
        cursor.execute(
            """
            INSERT INTO dig_gear
                (discord_id, guild_id, slot, tier, durability,
                 equipped, acquired_at, source)
            SELECT
                discord_id,
                guild_id,
                'weapon',
                pickaxe_tier,
                20,
                1,
                COALESCE(last_dig_at, CAST(strftime('%s', 'now') AS INTEGER)),
                'migration'
            FROM tunnels
            """
        )

    def _migration_backfill_missing_dig_weapon_gear(self, cursor) -> None:
        """Backfill a starter/current weapon row for tunnels with no weapon.

        The original ``create_dig_gear_system`` migration only ran once. Any
        tunnel created after that migration but before ``create_tunnel`` began
        inserting starter weapon rows could have a legacy ``pickaxe_tier`` with
        no matching ``dig_gear`` weapon row, leaving /dig gear's Weapon slot
        empty even though digging still used the pickaxe tier fallback.
        """
        cursor.execute(
            """
            INSERT INTO dig_gear
                (discord_id, guild_id, slot, tier, durability,
                 equipped, acquired_at, source)
            SELECT
                t.discord_id,
                COALESCE(t.guild_id, 0),
                'weapon',
                COALESCE(t.pickaxe_tier, 0),
                20,
                1,
                COALESCE(
                    t.last_dig_at,
                    t.created_at,
                    CAST(strftime('%s', 'now') AS INTEGER)
                ),
                'missing_weapon_backfill'
            FROM tunnels t
            WHERE NOT EXISTS (
                SELECT 1
                FROM dig_gear g
                WHERE g.discord_id = t.discord_id
                  AND g.guild_id = COALESCE(t.guild_id, 0)
                  AND g.slot = 'weapon'
            )
            """
        )

    def _migration_backfill_overgrowth_charges_remaining(self, cursor) -> None:
        """Grant 10 dig charges to legacy Overgrowth buffs missing the field."""
        now = int(time.time())
        rows = cursor.execute(
            """
            SELECT id, data
            FROM manashop_buffs
            WHERE buff_type = 'overgrowth'
              AND triggered = 0
              AND expires_at > ?
            """,
            (now,),
        ).fetchall()

        for row in rows:
            buff_id = row[0]
            raw_data = row[1]
            try:
                data = json.loads(raw_data or "{}")
            except (json.JSONDecodeError, TypeError):
                data = {}
            if not isinstance(data, dict):
                data = {}
            if "charges_remaining" in data:
                continue
            data["charges_remaining"] = 10
            cursor.execute(
                "UPDATE manashop_buffs SET data = ? WHERE id = ?",
                (json.dumps(data), buff_id),
            )

    def _migration_predictions_mini_split_v1(self, cursor) -> None:
        """10:1 stock split for prediction markets, fair-history table, banner sentinel.

        Quantity columns scale ×10; jopa and price columns are untouched. The
        contract value constant moves 100→10 in code so the same trade payload
        clears at the same jopa total. Combined with the per-trade VWAP rewrite,
        every existing position's P&L is preserved exactly.

        Also creates ``prediction_fair_snapshots`` (the new EOD-fair history
        feeding the per-market chart), backfills one row per still-open market,
        and writes a one-shot ``split_announced=0`` sentinel into ``app_kv`` so
        the next daily digest can post a one-time 'units restated' notice.
        """
        cursor.execute(
            "UPDATE prediction_positions "
            "SET yes_contracts = yes_contracts * 10, "
            "    no_contracts = no_contracts * 10"
        )
        cursor.execute(
            "UPDATE prediction_levels SET remaining_size = remaining_size * 10"
        )
        cursor.execute(
            "UPDATE prediction_trades SET contracts = contracts * 10"
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS prediction_fair_snapshots (
                snapshot_id INTEGER PRIMARY KEY AUTOINCREMENT,
                market_id   INTEGER NOT NULL,
                guild_id    INTEGER NOT NULL,
                snapshot_at INTEGER NOT NULL,
                fair_pct    INTEGER NOT NULL,
                reason      TEXT NOT NULL
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_prediction_fair_snapshots_lookup "
            "ON prediction_fair_snapshots(market_id, guild_id, snapshot_at)"
        )
        cursor.execute(
            """
            INSERT OR IGNORE INTO prediction_fair_snapshots
                (market_id, guild_id, snapshot_at, fair_pct, reason)
            SELECT
                prediction_id,
                guild_id,
                COALESCE(last_refresh_at, CAST(strftime('%s', 'now') AS INTEGER)),
                COALESCE(current_price, initial_fair),
                'backfill'
            FROM predictions
            WHERE status = 'open'
              AND COALESCE(current_price, initial_fair) IS NOT NULL
            """
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS app_kv (
                guild_id INTEGER NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (guild_id, key)
            )
            """
        )
        cursor.execute(
            """
            INSERT OR IGNORE INTO app_kv (guild_id, key, value)
            SELECT DISTINCT guild_id, 'split_announced', '0'
            FROM prediction_trades
            JOIN predictions USING (prediction_id)
            """
        )

    def _migration_predictions_fair_history_backfill_from_levels(self, cursor) -> None:
        """Backfill ``prediction_fair_snapshots`` from ``prediction_levels.posted_at``.

        The prior split migration only inserted one snapshot per still-open
        market, so charts for older markets render as a single flat point.
        ``prediction_levels.posted_at`` records when each level was layered in
        (the LP centers each refresh's ladder around the new fair), so for
        each (market, UTC day) we can derive a defensible historical fair
        from the levels posted that day.

        Dedupes against any pre-existing row at the same EOD timestamp via
        ``NOT EXISTS`` rather than a unique index — production writers can
        legitimately fire two snapshots within the same second (e.g. create
        + immediate refresh in tests), so the index stays non-unique.
        """
        # One snapshot per (market, UTC day) at end-of-day timestamp.
        # Both sides present → mid; single side → that side's best price.
        cursor.execute(
            """
            INSERT INTO prediction_fair_snapshots
                (market_id, guild_id, snapshot_at, fair_pct, reason)
            SELECT
                l.prediction_id,
                p.guild_id,
                CAST(strftime(
                    '%s',
                    date(l.posted_at, 'unixepoch'),
                    '+1 day',
                    '-1 second'
                ) AS INTEGER) AS eod_ts,
                CASE
                    WHEN MIN(CASE WHEN l.side = 'yes_ask' THEN l.price END) IS NOT NULL
                     AND MAX(CASE WHEN l.side = 'yes_bid' THEN l.price END) IS NOT NULL
                        THEN (MIN(CASE WHEN l.side = 'yes_ask' THEN l.price END)
                            + MAX(CASE WHEN l.side = 'yes_bid' THEN l.price END)) / 2
                    WHEN MIN(CASE WHEN l.side = 'yes_ask' THEN l.price END) IS NOT NULL
                        THEN MIN(CASE WHEN l.side = 'yes_ask' THEN l.price END)
                    ELSE MAX(CASE WHEN l.side = 'yes_bid' THEN l.price END)
                END,
                'backfill_levels'
            FROM prediction_levels l
            JOIN predictions p ON p.prediction_id = l.prediction_id
            GROUP BY l.prediction_id, date(l.posted_at, 'unixepoch')
            HAVING NOT EXISTS (
                SELECT 1 FROM prediction_fair_snapshots s
                WHERE s.market_id = l.prediction_id
                  AND s.guild_id = p.guild_id
                  AND s.snapshot_at = eod_ts
            )
            """
        )

    def _migration_add_last_cheer_at_to_tunnels(self, cursor) -> None:
        """Track the cheerer's last cheer timestamp so cheer can have its own
        short cooldown without sharing the free-dig cooldown.
        """
        self._add_column_if_not_exists(cursor, "tunnels", "last_cheer_at", "INTEGER")

    def _migration_create_reminder_preferences_table(self, cursor) -> None:
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS reminder_preferences (
                discord_id      INTEGER NOT NULL,
                guild_id        INTEGER NOT NULL DEFAULT 0,
                wheel_enabled   INTEGER NOT NULL DEFAULT 0,
                trivia_enabled  INTEGER NOT NULL DEFAULT 0,
                betting_enabled INTEGER NOT NULL DEFAULT 0,
                updated_at      INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_prefs_guild "
            "ON reminder_preferences(guild_id)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_prefs_wheel "
            "ON reminder_preferences(guild_id, wheel_enabled)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_prefs_trivia "
            "ON reminder_preferences(guild_id, trivia_enabled)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_prefs_betting "
            "ON reminder_preferences(guild_id, betting_enabled)"
        )

    def _migration_add_dig_enabled_to_reminder_preferences(self, cursor) -> None:
        self._add_column_if_not_exists(
            cursor, "reminder_preferences", "dig_enabled", "INTEGER NOT NULL DEFAULT 0"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_reminder_prefs_dig "
            "ON reminder_preferences(guild_id, dig_enabled)"
        )

    def _migration_drop_mana_shop_items_table(self, cursor) -> None:
        """Drop the mana_shop_items table. All write sites have been removed
        and no consumer code ever existed; the delayed-token wedges and shop
        items have been replaced with immediate-effect alternatives."""
        cursor.execute("DROP TABLE IF EXISTS mana_shop_items")

    def _migration_drop_mana_daily_losses_table(self, cursor) -> None:
        """Drop the mana_daily_losses table. The Regrowth shop item that read
        from it has been rewritten to use existing bet/wheel-spin history."""
        cursor.execute("DROP TABLE IF EXISTS mana_daily_losses")

    def _migration_renumber_pickaxe_tier_for_stormrend_insert(self, cursor) -> None:
        """A new tier ("Stormrend") was inserted between Obsidian (tier 4)
        and Frostforged. Existing player tunnels and equipped gear that
        reference the old tier indices need to shift up: 6 -> 7
        (Void-Touched) and 5 -> 6 (Frostforged). Order matters — bump
        the higher tier first so 5 -> 6 doesn't collide with the
        previous Void-Touched rows.
        """
        # tunnels.pickaxe_tier (legacy persistent column).
        cursor.execute("UPDATE tunnels SET pickaxe_tier = 7 WHERE pickaxe_tier = 6")
        cursor.execute("UPDATE tunnels SET pickaxe_tier = 6 WHERE pickaxe_tier = 5")
        # dig_gear.tier (per-piece, all three slots).
        cursor.execute("UPDATE dig_gear SET tier = 7 WHERE tier = 6")
        cursor.execute("UPDATE dig_gear SET tier = 6 WHERE tier = 5")

    def _migration_create_dig_dm_memory_table(self, cursor) -> None:
        """Create dig_dm_memory table for the DM's per-player narrative scratchpad."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS dig_dm_memory (
                discord_id   INTEGER NOT NULL,
                guild_id     INTEGER NOT NULL DEFAULT 0,
                summary_text TEXT NOT NULL DEFAULT '',
                updated_at   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_clear_dig_active_duels_for_retired_timed_mechanics(
        self, cursor,
    ) -> None:
        """Clear any in-flight duels paused on the retired arithmetic / riddle
        pinnacle mechanics. Without this, players whose duel was waiting on the
        text-input modal at deploy time would be stuck forever — the modal UI
        and the resolver branch both no longer exist.

        Idempotent: it's a DELETE filtered by mechanic_id, so a fresh DB or a
        DB that has none of these rows is a no-op.
        """
        cursor.execute(
            "DELETE FROM dig_active_duels "
            "WHERE mechanic_id IN ("
            "  'pinnacle_arithmetic_challenge', 'pinnacle_riddle_challenge'"
            ")"
        )

    def _migration_dig_buff_fun_charges(self, cursor) -> None:
        """Multi-charge Grappling Hook + pending Sonar Pulse skip flag.

        ``grappling_hook_charges`` mirrors ``hard_hat_charges``: each purchase
        grants N charges that consume on cave-in (zeroing block_loss + stun).
        ``sonar_skip_pending`` is a one-shot bool that causes the next
        triggered event to pass by harmlessly.
        """
        self._add_column_if_not_exists(
            cursor, "tunnels", "grappling_hook_charges", "INTEGER NOT NULL DEFAULT 0",
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "sonar_skip_pending", "INTEGER NOT NULL DEFAULT 0",
        )

    def _migration_relic_loadout_cap_and_streak(self, cursor) -> None:
        """Bound equipped relics at the new ceiling + add Prospector's Streak state.

        ``cavein_free_streak`` tracks consecutive cave-in-free digs (the
        Prospector's Streak relic reads it). ``relic_trim_notice`` is a one-shot
        bool: set for any player trimmed below, cleared after their next /dig
        surfaces the notice.

        Before relics were capped, equippable slots were ``prestige_level + 1``
        with no ceiling, so high-prestige players could equip more than the new
        max of 6. Flag those players (count computed on the pre-trim state), then
        unequip everything but their 6 most recently acquired relics (highest row
        id). Trimmed relics stay owned in inventory, just unequipped. The 6 here
        matches ``RELIC_SLOTS_MAX`` at the time of this one-time correction.
        """
        self._add_column_if_not_exists(
            cursor, "tunnels", "cavein_free_streak", "INTEGER NOT NULL DEFAULT 0",
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "relic_trim_notice", "INTEGER NOT NULL DEFAULT 0",
        )
        cursor.execute(
            """
            UPDATE tunnels SET relic_trim_notice = 1
            WHERE (
                SELECT COUNT(*) FROM dig_artifacts d
                WHERE d.is_relic = 1 AND d.equipped = 1
                  AND d.discord_id = tunnels.discord_id
                  AND d.guild_id = tunnels.guild_id
            ) > 6
            """
        )
        cursor.execute(
            """
            UPDATE dig_artifacts SET equipped = 0
            WHERE id IN (
                SELECT id FROM (
                    SELECT id, ROW_NUMBER() OVER (
                        PARTITION BY discord_id, guild_id ORDER BY id DESC
                    ) AS rn
                    FROM dig_artifacts WHERE is_relic = 1 AND equipped = 1
                ) WHERE rn > 6
            )
            """
        )

    def _migration_add_dig_auto_buy_settings(self, cursor) -> None:
        """Add per-miner auto-buy toggles for common dig consumables."""
        self._add_column_if_not_exists(
            cursor, "tunnels", "auto_buy_torch", "INTEGER NOT NULL DEFAULT 0",
        )
        self._add_column_if_not_exists(
            cursor, "tunnels", "auto_buy_hard_hat", "INTEGER NOT NULL DEFAULT 0",
        )

    def _migration_add_item_id_to_dig_gear(self, cursor) -> None:
        """Add an optional registry key for event-only unique gear."""
        self._add_column_if_not_exists(cursor, "dig_gear", "item_id", "TEXT")

    def _migration_create_economy_ledger_tables(self, cursor) -> None:
        """Create central money-movement ledger and balance-change triggers."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS economy_ledger_entries (
                ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                account_type TEXT NOT NULL CHECK(account_type IN ('player', 'nonprofit')),
                account_id INTEGER,
                delta INTEGER NOT NULL,
                balance_before INTEGER NOT NULL,
                balance_after INTEGER NOT NULL,
                source TEXT NOT NULL DEFAULT 'balance_update',
                actor_id INTEGER,
                related_type TEXT,
                related_id TEXT,
                reason TEXT,
                metadata TEXT,
                created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
            )
            """
        )
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS economy_ledger_context (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                source TEXT,
                actor_id INTEGER,
                related_type TEXT,
                related_id TEXT,
                reason TEXT,
                metadata TEXT
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_economy_ledger_guild_created
            ON economy_ledger_entries(guild_id, created_at DESC, ledger_id DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_economy_ledger_account
            ON economy_ledger_entries(guild_id, account_type, account_id, created_at DESC)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_economy_ledger_source
            ON economy_ledger_entries(guild_id, source, created_at DESC)
            """
        )

        cursor.execute(
            """
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source, reason
            )
            SELECT COALESCE(p.guild_id, 0), 'player', p.discord_id,
                   COALESCE(p.jopacoin_balance, 0), 0,
                   COALESCE(p.jopacoin_balance, 0),
                   'ledger_backfill', 'opening balance at ledger creation'
            FROM players p
            WHERE COALESCE(p.jopacoin_balance, 0) != 0
              AND NOT EXISTS (
                  SELECT 1
                  FROM economy_ledger_entries e
                  WHERE e.guild_id = COALESCE(p.guild_id, 0)
                    AND e.account_type = 'player'
                    AND e.account_id = p.discord_id
              )
            """
        )
        cursor.execute(
            """
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source, reason
            )
            SELECT COALESCE(n.guild_id, 0), 'nonprofit', COALESCE(n.guild_id, 0),
                   COALESCE(n.total_collected, 0), 0,
                   COALESCE(n.total_collected, 0),
                   'ledger_backfill', 'opening nonprofit fund at ledger creation'
            FROM nonprofit_fund n
            WHERE COALESCE(n.total_collected, 0) != 0
              AND NOT EXISTS (
                  SELECT 1
                  FROM economy_ledger_entries e
                  WHERE e.guild_id = COALESCE(n.guild_id, 0)
                    AND e.account_type = 'nonprofit'
                    AND e.account_id = COALESCE(n.guild_id, 0)
              )
            """
        )

        cursor.execute("DROP TRIGGER IF EXISTS trg_economy_ledger_players_insert")
        cursor.execute(
            """
            CREATE TRIGGER trg_economy_ledger_players_insert
            AFTER INSERT ON players
            WHEN COALESCE(NEW.jopacoin_balance, 0) != 0
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                    COALESCE(NEW.jopacoin_balance, 0), 0,
                    COALESCE(NEW.jopacoin_balance, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'player_insert'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END
            """
        )

        cursor.execute("DROP TRIGGER IF EXISTS trg_economy_ledger_players_update")
        cursor.execute(
            """
            CREATE TRIGGER trg_economy_ledger_players_update
            AFTER UPDATE OF jopacoin_balance ON players
            WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                    COALESCE(NEW.jopacoin_balance, 0) - COALESCE(OLD.jopacoin_balance, 0),
                    COALESCE(OLD.jopacoin_balance, 0),
                    COALESCE(NEW.jopacoin_balance, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END
            """
        )

        cursor.execute("DROP TRIGGER IF EXISTS trg_economy_ledger_nonprofit_insert")
        cursor.execute(
            """
            CREATE TRIGGER trg_economy_ledger_nonprofit_insert
            AFTER INSERT ON nonprofit_fund
            WHEN COALESCE(NEW.total_collected, 0) != 0
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                    COALESCE(NEW.total_collected, 0), 0,
                    COALESCE(NEW.total_collected, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_insert'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END
            """
        )

        cursor.execute("DROP TRIGGER IF EXISTS trg_economy_ledger_nonprofit_update")
        cursor.execute(
            """
            CREATE TRIGGER trg_economy_ledger_nonprofit_update
            AFTER UPDATE OF total_collected ON nonprofit_fund
            WHEN COALESCE(OLD.total_collected, 0) != COALESCE(NEW.total_collected, 0)
            BEGIN
                INSERT INTO economy_ledger_entries (
                    guild_id, account_type, account_id, delta,
                    balance_before, balance_after, source, actor_id,
                    related_type, related_id, reason, metadata
                )
                VALUES (
                    COALESCE(NEW.guild_id, 0), 'nonprofit', COALESCE(NEW.guild_id, 0),
                    COALESCE(NEW.total_collected, 0) - COALESCE(OLD.total_collected, 0),
                    COALESCE(OLD.total_collected, 0),
                    COALESCE(NEW.total_collected, 0),
                    COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'nonprofit_update'),
                    (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                    (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                    (SELECT reason FROM economy_ledger_context WHERE id = 1),
                    (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                );
            END
            """
        )

    def _migration_backfill_economy_ledger_opening_balances(self, cursor) -> None:
        """Backfill opening balances for DBs that already ran ledger creation."""
        cursor.execute(
            """
            WITH existing AS (
                SELECT guild_id, account_id, COALESCE(SUM(delta), 0) AS logged_delta
                FROM economy_ledger_entries
                WHERE account_type = 'player'
                GROUP BY guild_id, account_id
            ),
            openings AS (
                SELECT COALESCE(p.guild_id, 0) AS guild_id,
                       p.discord_id AS account_id,
                       COALESCE(p.jopacoin_balance, 0)
                           - COALESCE(e.logged_delta, 0) AS opening_balance
                FROM players p
                LEFT JOIN existing e
                  ON e.guild_id = COALESCE(p.guild_id, 0)
                 AND e.account_id = p.discord_id
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM economy_ledger_entries le
                    WHERE le.guild_id = COALESCE(p.guild_id, 0)
                      AND le.account_type = 'player'
                      AND le.account_id = p.discord_id
                      AND le.source = 'ledger_backfill'
                )
            )
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source, reason
            )
            SELECT guild_id, 'player', account_id, opening_balance,
                   0, opening_balance, 'ledger_backfill',
                   'opening balance at ledger creation'
            FROM openings
            WHERE opening_balance != 0
            """
        )
        cursor.execute(
            """
            WITH existing AS (
                SELECT guild_id, account_id, COALESCE(SUM(delta), 0) AS logged_delta
                FROM economy_ledger_entries
                WHERE account_type = 'nonprofit'
                GROUP BY guild_id, account_id
            ),
            openings AS (
                SELECT COALESCE(n.guild_id, 0) AS guild_id,
                       COALESCE(n.guild_id, 0) AS account_id,
                       COALESCE(n.total_collected, 0)
                           - COALESCE(e.logged_delta, 0) AS opening_balance
                FROM nonprofit_fund n
                LEFT JOIN existing e
                  ON e.guild_id = COALESCE(n.guild_id, 0)
                 AND e.account_id = COALESCE(n.guild_id, 0)
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM economy_ledger_entries le
                    WHERE le.guild_id = COALESCE(n.guild_id, 0)
                      AND le.account_type = 'nonprofit'
                      AND le.account_id = COALESCE(n.guild_id, 0)
                      AND le.source = 'ledger_backfill'
                )
            )
            INSERT INTO economy_ledger_entries (
                guild_id, account_type, account_id, delta,
                balance_before, balance_after, source, reason
            )
            SELECT guild_id, 'nonprofit', account_id, opening_balance,
                   0, opening_balance, 'ledger_backfill',
                   'opening nonprofit fund at ledger creation'
            FROM openings
            WHERE opening_balance != 0
            """
        )

    def _migration_create_tax_fine_cooldowns_table(self, cursor) -> None:
        """Track the latest successful Tax Man fine for each guild-scoped player."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS tax_fine_cooldowns (
                discord_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL DEFAULT 0,
                last_fined_at INTEGER NOT NULL,
                last_amount INTEGER NOT NULL DEFAULT 0,
                last_actor_id INTEGER,
                last_reason TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_tax_fine_cooldowns_guild_last
            ON tax_fine_cooldowns(guild_id, last_fined_at DESC)
            """
        )

    def _migration_create_protected_hero_purchases_table(self, cursor) -> None:
        """Store shop protect-hero purchases for pending matches."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS protected_hero_purchases (
                purchase_id INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id INTEGER NOT NULL,
                pending_match_id INTEGER NOT NULL,
                match_id INTEGER,
                discord_id INTEGER NOT NULL,
                team_side TEXT NOT NULL CHECK(team_side IN ('radiant', 'dire')),
                hero_id INTEGER NOT NULL,
                cost INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending', 'recorded', 'aborted')),
                purchased_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                resolved_at TIMESTAMP,
                UNIQUE(guild_id, pending_match_id, discord_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_protected_hero_purchases_player
            ON protected_hero_purchases(guild_id, discord_id, status)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_protected_hero_purchases_pending
            ON protected_hero_purchases(guild_id, pending_match_id)
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_protected_hero_purchases_match
            ON protected_hero_purchases(match_id)
            """
        )

    def _migration_drop_prediction_bets_table(self, cursor) -> None:
        """Drop the obsolete pari-mutuel prediction_bets table. The order-book
        prediction rework replaced it; all pool-mode read/write paths have
        been removed."""
        cursor.execute("DROP TABLE IF EXISTS prediction_bets")

    def _migration_create_mafia_tables(self, cursor) -> None:
        """Create all tables for the Daily Mafia subgame."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_games (
                game_id              INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id             INTEGER NOT NULL DEFAULT 0,
                game_date            TEXT    NOT NULL,
                phase                TEXT    NOT NULL,
                started_at           INTEGER NOT NULL,
                night_ended_at       INTEGER,
                day_ended_at         INTEGER,
                winner               TEXT,
                entry_fee            INTEGER NOT NULL DEFAULT 0,
                payout_per_winner    INTEGER NOT NULL DEFAULT 0,
                mvp_id               INTEGER,
                roster_size          INTEGER NOT NULL,
                twist_event          TEXT,
                mafia_thread_id      INTEGER,
                discussion_thread_id INTEGER,
                setup_message_id     INTEGER,
                UNIQUE(guild_id, game_date)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_games_guild_phase ON mafia_games(guild_id, phase)"
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_players (
                game_id          INTEGER NOT NULL,
                discord_id       INTEGER NOT NULL,
                guild_id         INTEGER NOT NULL DEFAULT 0,
                role             TEXT    NOT NULL,
                is_godfather     INTEGER NOT NULL DEFAULT 0,
                hero_name        TEXT,
                is_alive         INTEGER NOT NULL DEFAULT 1,
                eliminated_phase TEXT,
                eliminated_at    INTEGER,
                acted            INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (game_id, discord_id),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_players_lookup ON mafia_players(guild_id, discord_id)"
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_actions (
                action_id   INTEGER PRIMARY KEY AUTOINCREMENT,
                game_id     INTEGER NOT NULL,
                guild_id    INTEGER NOT NULL DEFAULT 0,
                actor_id    INTEGER NOT NULL,
                target_id   INTEGER,
                action_type TEXT NOT NULL,
                phase       TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                result      TEXT,
                UNIQUE(game_id, actor_id, action_type, phase)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_actions_game_type ON mafia_actions(game_id, action_type)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_actions_actor ON mafia_actions(game_id, actor_id)"
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_optout (
                discord_id INTEGER NOT NULL,
                guild_id   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (discord_id, guild_id)
            )
            """
        )

    def _migration_add_mafia_entry_fee_column(self, cursor) -> None:
        """Persist the entry fee charged per mafia game so audits can reconstruct
        the pot used to compute payouts. Idempotent for fresh installs where the
        column already appears in _migration_create_mafia_tables.
        """
        self._add_column_if_not_exists(
            cursor, "mafia_games", "entry_fee", "INTEGER NOT NULL DEFAULT 0"
        )

    def _migration_add_mafia_weekly_game_columns(self, cursor) -> None:
        """Week-long redesign: a game now spans multiple night/day cycles.

        - day_number: 1-based cycle counter.
        - phase_started_at: start of the current phase (drives the per-cycle
          clock); backfilled to started_at for in-flight rows.
        - standings_message_id / graveyard_thread_id: visibility surfaces.
        - status: ACTIVE / CANCELLED (admin abort).
        """
        self._add_column_if_not_exists(
            cursor, "mafia_games", "day_number", "INTEGER NOT NULL DEFAULT 1"
        )
        self._add_column_if_not_exists(
            cursor, "mafia_games", "phase_started_at", "INTEGER"
        )
        self._add_column_if_not_exists(
            cursor, "mafia_games", "standings_message_id", "INTEGER"
        )
        self._add_column_if_not_exists(
            cursor, "mafia_games", "graveyard_thread_id", "INTEGER"
        )
        self._add_column_if_not_exists(
            cursor, "mafia_games", "status", "TEXT NOT NULL DEFAULT 'ACTIVE'"
        )
        cursor.execute(
            "UPDATE mafia_games SET phase_started_at = started_at "
            "WHERE phase_started_at IS NULL"
        )

    def _migration_rebuild_mafia_actions_per_night(self, cursor) -> None:
        """Re-key mafia_actions per cycle.

        The original UNIQUE(game_id, actor_id, action_type, phase) prevents a
        second night's KILL/INVESTIGATE/etc. With a week-long game, actions must
        be unique per cycle, so the constraint becomes
        UNIQUE(game_id, actor_id, action_type, phase, day_number). SQLite can't
        drop a table-level UNIQUE in place, so rebuild the table. Guarded by the
        presence of the day_number column so it only runs once.
        """
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='mafia_actions'")
        if not cursor.fetchone():
            return
        cursor.execute("PRAGMA table_info(mafia_actions)")
        existing_cols = {row["name"] for row in cursor.fetchall()}
        if "day_number" in existing_cols:
            return

        cursor.execute(
            """
            CREATE TABLE mafia_actions_new (
                action_id   INTEGER PRIMARY KEY AUTOINCREMENT,
                game_id     INTEGER NOT NULL,
                guild_id    INTEGER NOT NULL DEFAULT 0,
                actor_id    INTEGER NOT NULL,
                target_id   INTEGER,
                action_type TEXT NOT NULL,
                phase       TEXT NOT NULL,
                day_number  INTEGER NOT NULL DEFAULT 1,
                created_at  INTEGER NOT NULL,
                result      TEXT,
                UNIQUE(game_id, actor_id, action_type, phase, day_number)
            )
            """
        )
        cursor.execute(
            """
            INSERT INTO mafia_actions_new
                (action_id, game_id, guild_id, actor_id, target_id,
                 action_type, phase, day_number, created_at, result)
            SELECT action_id, game_id, guild_id, actor_id, target_id,
                   action_type, phase, 1, created_at, result
            FROM mafia_actions
            """
        )
        cursor.execute("DROP TABLE mafia_actions")
        cursor.execute("ALTER TABLE mafia_actions_new RENAME TO mafia_actions")
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_actions_game_type ON mafia_actions(game_id, action_type)"
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_actions_actor ON mafia_actions(game_id, actor_id)"
        )

    def _migration_create_mafia_bounties_table(self, cursor) -> None:
        """Town Bounty: living players stake at most 1 JC per suspect per day.

        The composite PRIMARY KEY enforces the one-stake-per-contributor-per-
        target-per-day rule at the storage layer.
        """
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_bounties (
                game_id        INTEGER NOT NULL,
                guild_id       INTEGER NOT NULL DEFAULT 0,
                day_number     INTEGER NOT NULL,
                target_id      INTEGER NOT NULL,
                contributor_id INTEGER NOT NULL,
                amount         INTEGER NOT NULL DEFAULT 1,
                created_at     INTEGER NOT NULL,
                PRIMARY KEY (game_id, day_number, target_id, contributor_id),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id)
            )
            """
        )
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_bounties_day "
            "ON mafia_bounties(game_id, day_number)"
        )

    def _migration_create_mafia_meta_table(self, cursor) -> None:
        """Per-guild mafia metadata (e.g. the pinned hall-of-fame message id)."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_meta (
                guild_id                INTEGER PRIMARY KEY DEFAULT 0,
                hall_of_fame_message_id INTEGER,
                updated_at              TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )
            """
        )

    def _migration_create_mafia_signups_table(self, cursor) -> None:
        """Opt-in roster priority: players who /mafia join for a given week."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_signups (
                guild_id   INTEGER NOT NULL DEFAULT 0,
                week_start TEXT    NOT NULL,
                discord_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (guild_id, week_start, discord_id)
            )
            """
        )

    def _migration_create_mafia_phase_reminders_table(self, cursor) -> None:
        """Durable once-per-cycle claims for automatic Mafia reminders."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mafia_phase_reminders (
                guild_id   INTEGER NOT NULL DEFAULT 0,
                game_id     INTEGER NOT NULL,
                day_number  INTEGER NOT NULL,
                phase       TEXT    NOT NULL,
                claimed_at  INTEGER NOT NULL,
                PRIMARY KEY (guild_id, game_id, day_number, phase),
                FOREIGN KEY (game_id) REFERENCES mafia_games(game_id) ON DELETE CASCADE
            )
            """
        )

    def _migration_rebuild_mafia_games_drop_date_unique(self, cursor) -> None:
        """Drop UNIQUE(guild_id, game_date) so games can start back-to-back.

        With the continuous cadence, a finished game's start-date can repeat for
        the next game the same day. SQLite can't drop a table-level UNIQUE in
        place, so rebuild. Guarded by the constraint still being present.
        """
        cursor.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='mafia_games'")
        if not cursor.fetchone():
            return
        cursor.execute("SELECT sql FROM sqlite_master WHERE type='table' AND name='mafia_games'")
        row = cursor.fetchone()
        if row is None or "UNIQUE(guild_id, game_date)" not in row[0]:
            return  # already rebuilt

        cursor.execute(
            """
            CREATE TABLE mafia_games_new (
                game_id              INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id             INTEGER NOT NULL DEFAULT 0,
                game_date            TEXT    NOT NULL,
                phase                TEXT    NOT NULL,
                started_at           INTEGER NOT NULL,
                night_ended_at       INTEGER,
                day_ended_at         INTEGER,
                winner               TEXT,
                entry_fee            INTEGER NOT NULL DEFAULT 0,
                payout_per_winner    INTEGER NOT NULL DEFAULT 0,
                mvp_id               INTEGER,
                roster_size          INTEGER NOT NULL,
                twist_event          TEXT,
                mafia_thread_id      INTEGER,
                discussion_thread_id INTEGER,
                setup_message_id     INTEGER,
                day_number           INTEGER NOT NULL DEFAULT 1,
                phase_started_at     INTEGER,
                standings_message_id INTEGER,
                graveyard_thread_id  INTEGER,
                status               TEXT NOT NULL DEFAULT 'ACTIVE'
            )
            """
        )
        cursor.execute(
            """
            INSERT INTO mafia_games_new (
                game_id, guild_id, game_date, phase, started_at, night_ended_at,
                day_ended_at, winner, entry_fee, payout_per_winner, mvp_id,
                roster_size, twist_event, mafia_thread_id, discussion_thread_id,
                setup_message_id, day_number, phase_started_at,
                standings_message_id, graveyard_thread_id, status
            )
            SELECT
                game_id, guild_id, game_date, phase, started_at, night_ended_at,
                day_ended_at, winner, entry_fee, payout_per_winner, mvp_id,
                roster_size, twist_event, mafia_thread_id, discussion_thread_id,
                setup_message_id, day_number, phase_started_at,
                standings_message_id, graveyard_thread_id, status
            FROM mafia_games
            """
        )
        cursor.execute("DROP TABLE mafia_games")
        cursor.execute("ALTER TABLE mafia_games_new RENAME TO mafia_games")
        cursor.execute(
            "CREATE INDEX IF NOT EXISTS idx_mafia_games_guild_phase ON mafia_games(guild_id, phase)"
        )

    def _migration_add_white_shield_remaining_to_player_mana(self, cursor) -> None:
        """Add the per-mana-day capacity used by White's Guardian ward."""
        self._add_column_if_not_exists(
            cursor,
            "player_mana",
            "white_shield_remaining",
            "INTEGER NOT NULL DEFAULT 0",
        )
        # Preserve protection for a Plains player who already claimed today's
        # mana before this migration deployed. Stale rows are harmless because
        # ProtectionService also requires assigned_date == the active mana day.
        cursor.execute(
            """
            UPDATE player_mana
            SET white_shield_remaining = 25
            WHERE current_land = 'Plains' AND consumed_today = 0
              AND white_shield_remaining = 0
            """
        )

    def _migration_create_mana_protection_tables(self, cursor) -> None:
        """Create idempotent hostile-loss and protection-consumption ledgers."""
        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS hostile_loss_events (
                event_id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                guild_id                   INTEGER NOT NULL DEFAULT 0,
                victim_id                  INTEGER NOT NULL,
                actor_id                   INTEGER,
                event_key                  TEXT NOT NULL,
                kind                       TEXT NOT NULL,
                destination                TEXT NOT NULL
                                               CHECK(destination IN ('burn', 'player', 'reserve')),
                recipient_id               INTEGER,
                requested                  INTEGER NOT NULL CHECK(requested >= 0),
                attempted                  INTEGER NOT NULL CHECK(attempted >= 0),
                absorbed                   INTEGER NOT NULL CHECK(absorbed >= 0),
                applied                    INTEGER NOT NULL CHECK(applied >= 0),
                victim_balance_before      INTEGER NOT NULL,
                victim_balance_after       INTEGER NOT NULL,
                destination_balance_before INTEGER,
                destination_balance_after  INTEGER,
                shieldable                 INTEGER NOT NULL DEFAULT 1,
                retro_covered              INTEGER NOT NULL DEFAULT 0,
                protection_details         TEXT NOT NULL DEFAULT '[]',
                metadata                   TEXT,
                occurred_at                INTEGER NOT NULL,
                created_at                 INTEGER NOT NULL,
                UNIQUE(guild_id, victim_id, event_key)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_hostile_loss_retro
            ON hostile_loss_events(
                guild_id, victim_id, shieldable, occurred_at, event_id
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_hostile_loss_kind
            ON hostile_loss_events(guild_id, kind, occurred_at)
            """
        )

        cursor.execute(
            """
            CREATE TABLE IF NOT EXISTS mana_protection_events (
                protection_event_id INTEGER PRIMARY KEY AUTOINCREMENT,
                hostile_loss_event_id INTEGER NOT NULL,
                guild_id              INTEGER NOT NULL DEFAULT 0,
                victim_id             INTEGER NOT NULL,
                protection_type       TEXT NOT NULL,
                pool_key              TEXT NOT NULL,
                buff_id               INTEGER,
                amount                INTEGER NOT NULL CHECK(amount >= 0),
                rate                  REAL NOT NULL,
                capacity_before       INTEGER,
                capacity_after        INTEGER,
                retroactive           INTEGER NOT NULL DEFAULT 0,
                created_at            INTEGER NOT NULL,
                details               TEXT,
                UNIQUE(hostile_loss_event_id, pool_key, retroactive),
                FOREIGN KEY(hostile_loss_event_id)
                    REFERENCES hostile_loss_events(event_id)
            )
            """
        )
        cursor.execute(
            """
            CREATE INDEX IF NOT EXISTS idx_mana_protection_victim
            ON mana_protection_events(guild_id, victim_id, created_at)
            """
        )
