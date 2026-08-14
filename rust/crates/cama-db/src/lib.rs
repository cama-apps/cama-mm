//! SQLite migration authority, compatibility checks, and repositories for the
//! Rust lift-and-shift runtime.
//!
//! The Rust startup schema manager now owns migration during the runtime
//! cutover. Repository connections still use the shared SQLite compatibility
//! policy, and the audit proves that every declared migration has been applied.
//! Historical ledger rows that are no longer declared remain tolerated.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

pub mod admin;
pub mod autobet_investments;
#[path = "bankruptcy.rs"]
pub mod bankruptcy_repository;
#[path = "betting_service.rs"]
pub mod betting_service_repository;
pub mod blame_luke;
#[path = "core_repositories.rs"]
pub mod core_repositories;
pub mod dig_abandon_runtime;
pub mod dig_action_history;
pub mod dig_blood_pact;
#[path = "dig_bonus_events.rs"]
pub mod dig_bonus_events_repository;
pub mod dig_boss_runtime;
pub mod dig_carry_wager;
pub mod dig_event_runtime;
pub mod dig_event_threats;
pub mod dig_flavor_repository;
pub mod dig_gear_runtime;
pub mod dig_guild_modifiers;
#[path = "dig_inventory.rs"]
pub mod dig_inventory_repository;
pub mod dig_migration_contracts;
pub mod dig_miner_runtime;
#[path = "dig_new_events_repository.rs"]
pub mod dig_new_events_repository;
pub mod dig_prestige4_content;
pub mod dig_prestige_runtime;
pub mod dig_quest_repository;
pub mod dig_relic_recycling;
pub mod dig_relic_rework;
#[path = "dig_routes.rs"]
pub mod dig_routes_repository;
pub mod dig_social_runtime;
pub mod dig_splash_runtime;
#[path = "dig_sweep_fixes.rs"]
pub mod dig_sweep_fixes_repository;
#[path = "dig_tunnel_encounters.rs"]
pub mod dig_tunnel_encounters_repository;
pub mod dig_weather;
pub mod disbursement;
#[path = "dota_bet_seed.rs"]
pub mod dota_bet_seed_repository;
#[path = "dota_streak.rs"]
pub mod dota_streak_repository;
pub mod draft_state;
#[path = "duel.rs"]
pub mod duel_repository;
#[path = "economy_events.rs"]
pub mod economy_event_repository;
#[path = "gambling_stats.rs"]
pub mod gambling_stats_repository;
pub mod golden_wheel_repository;
#[path = "guild_config.rs"]
pub mod guild_config_repository;
#[path = "herogrid.rs"]
pub mod herogrid_repository;
pub mod llm_request;
#[path = "loan.rs"]
pub mod loan_repository;
#[path = "low_priority.rs"]
pub mod low_priority_repository;
#[path = "mafia_repository.rs"]
pub mod mafia_repository;
pub mod mana_protection;
#[path = "mana_service.rs"]
pub mod mana_service_repository;
#[path = "manashop_rework.rs"]
pub mod manashop_rework_repository;
#[path = "match_correction.rs"]
pub mod match_correction_repository;
#[path = "match_discovery.rs"]
pub mod match_discovery_repository;
#[path = "match_recording.rs"]
pub mod match_recording_repository;
pub mod match_runtime;
pub mod match_voting;
pub mod moderation;
pub mod neon_events;
pub mod notifications;
pub mod opendota_player;
#[path = "package_deal.rs"]
pub mod package_deal_repository;
#[path = "pairings.rs"]
pub mod pairings_repository;
pub mod pending_lobby;
#[path = "pet_brawl.rs"]
pub mod pet_brawl_repository;
#[path = "pet_eating.rs"]
pub mod pet_eating_repository;
#[path = "pet_evolution.rs"]
pub mod pet_evolution_repository;
pub mod pet_migration_contracts;
#[path = "pet.rs"]
pub mod pet_repository;
pub mod player_trivia;
#[path = "prediction_resolution.rs"]
pub mod prediction_resolution_repository;
#[path = "prediction_workers.rs"]
pub mod prediction_worker_repository;
#[path = "predictions.rs"]
pub mod predictions_repository;
pub mod rating_analysis;
#[path = "rating_history.rs"]
pub mod rating_history_repository;
#[path = "readycheck.rs"]
pub mod readycheck_repository;
pub mod referrals;
#[path = "registration.rs"]
pub mod registration_repository;
#[path = "reminders.rs"]
pub mod reminder_repository;
pub mod schema_manager;
pub mod schema_manager_contracts;
#[path = "scout.rs"]
pub mod scout_repository;
pub mod shop_runtime;
#[path = "soft_avoid.rs"]
pub mod soft_avoid_repository;
pub mod survey;
#[path = "tax.rs"]
pub mod tax_repository;
#[path = "tip.rs"]
pub mod tip_repository;
#[path = "trivia_commands.rs"]
pub mod trivia_commands_repository;
#[path = "vanity_tax.rs"]
pub mod vanity_tax_repository;
#[path = "wheel_spins.rs"]
pub mod wheel_spin_repository;
pub mod wrapped_live;
#[path = "wrapped.rs"]
pub mod wrapped_repository;

/// The migration names exported from Python's `SchemaManager._get_migrations`.
///
/// `xtask parity` verifies this file against the Python source on every CI run.
pub const EXPECTED_MIGRATIONS_TEXT: &str = include_str!("../../../schema/expected_migrations.txt");

const REQUIRED_TABLES: &[&str] = &[
    "app_kv",
    "bankruptcy_state",
    "bet_settlement_taxes",
    "bets",
    "dig_active_duels",
    "dig_artifacts",
    "dig_actions",
    "dig_boss_echoes",
    "dig_gear",
    "dig_inventory",
    "double_or_nothing_spins",
    "duel_challenges",
    "economy_daily_events",
    "economy_daily_snapshots",
    "economy_ledger_context",
    "economy_ledger_entries",
    "economy_policy_state",
    "first_game_pool_claims",
    "first_game_pool_funding_days",
    "guild_config",
    "loan_state",
    "llm_request_attempts",
    "lobby_state",
    "lobby_suspensions",
    "low_priority_state",
    "mafia_actions",
    "mafia_bounties",
    "mafia_games",
    "mafia_optout",
    "mafia_phase_reminders",
    "mafia_players",
    "mafia_signups",
    "manashop_buffs",
    "manashop_daily_uses",
    "manashop_purchases",
    "match_bans",
    "match_correction_claims",
    "match_corrections",
    "match_participants",
    "match_predictions",
    "matches",
    "moderation_events",
    "nonprofit_fund",
    "openskill_rating_events",
    "openskill_rating_revisions",
    "openskill_replay_jobs",
    "package_deal_purchases",
    "package_deals",
    "pending_matches",
    "player_mana",
    "player_pairings",
    "player_steam_ids",
    "players",
    "pet_accessories",
    "pet_brawls",
    "pet_evolution_daily",
    "pet_refund_windows",
    "pet_supplies",
    "pets",
    "reminder_preferences",
    "prediction_fair_snapshots",
    "prediction_levels",
    "prediction_positions",
    "prediction_trades",
    "predictions",
    "rating_history",
    "referrals",
    "schema_migrations",
    "slow_drip_claims",
    "soft_avoids",
    "tip_transactions",
    "trivia_sessions",
    "tunnels",
    "vanity_tax_enforcements",
    "wrapped_enrichment_facts",
    "wheel_spins",
];

/// A non-mutating report describing whether Rust can safely consume a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseAudit {
    pub path: PathBuf,
    pub quick_check: String,
    pub journal_mode: String,
    pub foreign_keys_enabled: bool,
    pub user_version: i64,
    pub applied_migration_count: usize,
    pub required_migration_count: usize,
    pub extra_historical_migrations: Vec<String>,
    pub missing_migrations: Vec<String>,
    pub missing_tables: Vec<String>,
}

impl DatabaseAudit {
    /// Whether the database matches the Python runtime's current storage contract.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.quick_check.eq_ignore_ascii_case("ok")
            && self.journal_mode.eq_ignore_ascii_case("wal")
            && !self.foreign_keys_enabled
            && self.user_version == 0
            && self.missing_migrations.is_empty()
            && self.missing_tables.is_empty()
    }

    /// Human-readable incompatibilities suitable for a preflight log.
    #[must_use]
    pub fn issues(&self) -> Vec<String> {
        let mut issues = Vec::new();
        if !self.quick_check.eq_ignore_ascii_case("ok") {
            issues.push(format!(
                "SQLite quick_check returned {:?}",
                self.quick_check
            ));
        }
        if !self.journal_mode.eq_ignore_ascii_case("wal") {
            issues.push(format!(
                "journal_mode is {:?}, expected WAL",
                self.journal_mode
            ));
        }
        if self.foreign_keys_enabled {
            issues.push("foreign key enforcement is enabled, unlike Python".to_owned());
        }
        if self.user_version != 0 {
            issues.push(format!(
                "PRAGMA user_version is {}, expected 0",
                self.user_version
            ));
        }
        if !self.missing_migrations.is_empty() {
            issues.push(format!(
                "missing migrations: {}",
                self.missing_migrations.join(", ")
            ));
        }
        if !self.missing_tables.is_empty() {
            issues.push(format!(
                "missing tables: {}",
                self.missing_tables.join(", ")
            ));
        }
        issues
    }
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("database path does not exist or is not a file: {0}")]
    MissingDatabase(PathBuf),
    #[error("SQLite compatibility check failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Open an existing database for normal repository reads and writes.
///
/// Schema creation and migration are handled by the startup schema manager,
/// never by repository construction. The explicit foreign-key pragma
/// compensates for rusqlite's bundled SQLite default so repository behavior
/// matches the former Python runtime (`OFF`).
pub(crate) fn open_runtime_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    Ok(connection)
}

/// Return the ordered, unique migration contract embedded in the Rust build.
#[must_use]
pub fn expected_migrations() -> Vec<&'static str> {
    EXPECTED_MIGRATIONS_TEXT
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Audit an existing SQLite database without performing logical writes.
///
/// SQLite may still coordinate through an existing `-shm` sidecar when the
/// database is in WAL mode. The database itself is opened with `READ_ONLY` and
/// the connection is also placed in `query_only` mode.
pub fn audit_database(path: impl AsRef<Path>) -> Result<DatabaseAudit, AuditError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(AuditError::MissingDatabase(path.to_path_buf()));
    }

    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    // Bundled SQLite enables foreign keys by default at compile time, while
    // Python's runtime intentionally leaves them disabled. This is a
    // connection-local setting and does not mutate database contents.
    connection.pragma_update(None, "foreign_keys", false)?;
    connection.pragma_update(None, "query_only", true)?;
    audit_connection(path, &connection)
}

fn audit_connection(path: &Path, connection: &Connection) -> Result<DatabaseAudit, AuditError> {
    let quick_check = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    let journal_mode = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let foreign_keys_enabled =
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))? != 0;
    let user_version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    let applied_migrations = query_string_set(
        connection,
        "SELECT name FROM schema_migrations ORDER BY name",
    )?;
    let required_migrations: BTreeSet<String> = expected_migrations()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let missing_migrations = required_migrations
        .difference(&applied_migrations)
        .cloned()
        .collect();
    let extra_historical_migrations = applied_migrations
        .difference(&required_migrations)
        .cloned()
        .collect();

    let existing_tables = query_string_set(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )?;
    let missing_tables = REQUIRED_TABLES
        .iter()
        .filter(|table| !existing_tables.contains(**table))
        .map(|table| (*table).to_owned())
        .collect();

    Ok(DatabaseAudit {
        path: path.to_path_buf(),
        quick_check,
        journal_mode,
        foreign_keys_enabled,
        user_version,
        applied_migration_count: applied_migrations.len(),
        required_migration_count: required_migrations.len(),
        extra_historical_migrations,
        missing_migrations,
        missing_tables,
    })
}

fn query_string_set(
    connection: &Connection,
    sql: &str,
) -> Result<BTreeSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<BTreeSet<_>, _>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn assert_runtime_connection_reuses_schema_sqlite_configuration() {
        let file = NamedTempFile::new().expect("create runtime connection database");
        crate::schema_manager::initialize_or_migrate(file.path())
            .expect("initialize runtime connection database");
        let connection = open_runtime_connection(file.path()).expect("open runtime connection");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal mode");
        let busy_timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("read busy timeout");
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign-key policy");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(busy_timeout, 5_000);
        assert_eq!(foreign_keys, 0);
    }

    fn compatible_database(extra_legacy_migration: bool) -> NamedTempFile {
        let file = NamedTempFile::new().expect("create temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open temporary SQLite database");
        connection
            .execute_batch(
                "
                PRAGMA journal_mode=WAL;
                CREATE TABLE dig_actions (id INTEGER PRIMARY KEY);
                CREATE TABLE dig_boss_echoes (id INTEGER PRIMARY KEY);
                CREATE TABLE dig_gear (id INTEGER PRIMARY KEY);
                CREATE TABLE dig_inventory (id INTEGER PRIMARY KEY);
                CREATE TABLE double_or_nothing_spins (id INTEGER PRIMARY KEY);
                CREATE TABLE duel_challenges (id INTEGER PRIMARY KEY);
                CREATE TABLE app_kv (id INTEGER PRIMARY KEY);
                CREATE TABLE bankruptcy_state (id INTEGER PRIMARY KEY);
                CREATE TABLE bet_settlement_taxes (id INTEGER PRIMARY KEY);
                CREATE TABLE bets (bet_id INTEGER PRIMARY KEY);
                CREATE TABLE dig_active_duels (id INTEGER PRIMARY KEY);
                CREATE TABLE dig_artifacts (id INTEGER PRIMARY KEY);
                CREATE TABLE schema_migrations (
                    name TEXT PRIMARY KEY,
                    applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                );
                CREATE TABLE players (discord_id INTEGER PRIMARY KEY);
                CREATE TABLE matches (match_id INTEGER PRIMARY KEY);
                CREATE TABLE match_bans (id INTEGER PRIMARY KEY);
                CREATE TABLE match_correction_claims (id INTEGER PRIMARY KEY);
                CREATE TABLE match_corrections (id INTEGER PRIMARY KEY);
                CREATE TABLE match_participants (match_id INTEGER, discord_id INTEGER);
                CREATE TABLE match_predictions (id INTEGER PRIMARY KEY);
                CREATE TABLE rating_history (id INTEGER PRIMARY KEY);
                CREATE TABLE referrals (id INTEGER PRIMARY KEY);
                CREATE TABLE lobby_state (id INTEGER PRIMARY KEY);
                CREATE TABLE lobby_suspensions (id INTEGER PRIMARY KEY);
                CREATE TABLE moderation_events (id INTEGER PRIMARY KEY);
                CREATE TABLE openskill_rating_events (id INTEGER PRIMARY KEY);
                CREATE TABLE openskill_rating_revisions (id INTEGER PRIMARY KEY);
                CREATE TABLE openskill_replay_jobs (id INTEGER PRIMARY KEY);
                CREATE TABLE pending_matches (id INTEGER PRIMARY KEY);
                CREATE TABLE nonprofit_fund (guild_id INTEGER PRIMARY KEY);
                CREATE TABLE economy_daily_events (event_id INTEGER PRIMARY KEY);
                CREATE TABLE economy_daily_snapshots (guild_id INTEGER PRIMARY KEY);
                CREATE TABLE economy_ledger_context (singleton INTEGER PRIMARY KEY);
                CREATE TABLE economy_ledger_entries (entry_id INTEGER PRIMARY KEY);
                CREATE TABLE economy_policy_state (guild_id INTEGER PRIMARY KEY);
                CREATE TABLE first_game_pool_claims (id INTEGER PRIMARY KEY);
                CREATE TABLE first_game_pool_funding_days (id INTEGER PRIMARY KEY);
                CREATE TABLE guild_config (guild_id INTEGER PRIMARY KEY);
                CREATE TABLE low_priority_state (
                    discord_id INTEGER,
                    guild_id INTEGER,
                    PRIMARY KEY (discord_id, guild_id)
                );
                CREATE TABLE mafia_actions (id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_bounties (id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_games (game_id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_optout (id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_phase_reminders (id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_players (id INTEGER PRIMARY KEY);
                CREATE TABLE mafia_signups (id INTEGER PRIMARY KEY);
                CREATE TABLE manashop_buffs (id INTEGER PRIMARY KEY);
                CREATE TABLE manashop_daily_uses (id INTEGER PRIMARY KEY);
                CREATE TABLE manashop_purchases (id INTEGER PRIMARY KEY);
                CREATE TABLE loan_state (discord_id INTEGER, guild_id INTEGER);
                CREATE TABLE llm_request_attempts (attempt_id INTEGER PRIMARY KEY);
                CREATE TABLE package_deals (id INTEGER PRIMARY KEY);
                CREATE TABLE package_deal_purchases (id INTEGER PRIMARY KEY);
                CREATE TABLE player_pairings (id INTEGER PRIMARY KEY);
                CREATE TABLE player_mana (id INTEGER PRIMARY KEY);
                CREATE TABLE player_steam_ids (id INTEGER PRIMARY KEY);
                CREATE TABLE pet_accessories (id INTEGER PRIMARY KEY);
                CREATE TABLE pet_brawls (brawl_id INTEGER PRIMARY KEY);
                CREATE TABLE pet_evolution_daily (id INTEGER PRIMARY KEY);
                CREATE TABLE pet_refund_windows (id INTEGER PRIMARY KEY);
                CREATE TABLE pet_supplies (id INTEGER PRIMARY KEY);
                CREATE TABLE pets (pet_id INTEGER PRIMARY KEY);
                CREATE TABLE reminder_preferences (discord_id INTEGER PRIMARY KEY);
                CREATE TABLE prediction_fair_snapshots (id INTEGER PRIMARY KEY);
                CREATE TABLE prediction_levels (id INTEGER PRIMARY KEY);
                CREATE TABLE prediction_positions (id INTEGER PRIMARY KEY);
                CREATE TABLE prediction_trades (id INTEGER PRIMARY KEY);
                CREATE TABLE predictions (id INTEGER PRIMARY KEY);
                CREATE TABLE slow_drip_claims (id INTEGER PRIMARY KEY);
                CREATE TABLE soft_avoids (id INTEGER PRIMARY KEY);
                CREATE TABLE tip_transactions (id INTEGER PRIMARY KEY);
                CREATE TABLE trivia_sessions (id INTEGER PRIMARY KEY);
                CREATE TABLE tunnels (id INTEGER PRIMARY KEY);
                CREATE TABLE vanity_tax_enforcements (id INTEGER PRIMARY KEY);
                CREATE TABLE wrapped_enrichment_facts (match_id INTEGER PRIMARY KEY);
                CREATE TABLE wheel_spins (spin_id INTEGER PRIMARY KEY);
                ",
            )
            .expect("create compatibility schema");
        for migration in expected_migrations() {
            connection
                .execute(
                    "INSERT INTO schema_migrations(name) VALUES (?)",
                    [migration],
                )
                .expect("insert required migration");
        }
        if extra_legacy_migration {
            connection
                .execute(
                    "INSERT INTO schema_migrations(name) VALUES ('retired_legacy_migration')",
                    [],
                )
                .expect("insert historical migration");
        }
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint test database");
        drop(connection);
        file
    }

    #[test]
    fn current_python_schema_is_compatible() {
        let file = compatible_database(false);
        let audit = audit_database(file.path()).expect("audit compatible database");

        assert!(audit.is_compatible(), "{:?}", audit.issues());
        assert_eq!(audit.required_migration_count, expected_migrations().len());
        assert!(audit.extra_historical_migrations.is_empty());
    }

    #[test]
    fn historical_migration_rows_are_tolerated() {
        let file = compatible_database(true);
        let audit = audit_database(file.path()).expect("audit database with retired migration");

        assert!(audit.is_compatible(), "{:?}", audit.issues());
        assert_eq!(
            audit.extra_historical_migrations,
            ["retired_legacy_migration"]
        );
    }

    #[test]
    fn missing_current_migration_blocks_compatibility() {
        let file = compatible_database(false);
        let connection = Connection::open(file.path()).expect("open temporary SQLite database");
        connection
            .execute(
                "DELETE FROM schema_migrations WHERE name = ?",
                [expected_migrations()[0]],
            )
            .expect("remove one migration");
        drop(connection);

        let audit = audit_database(file.path()).expect("audit stale database");
        assert!(!audit.is_compatible());
        assert_eq!(audit.missing_migrations, [expected_migrations()[0]]);
    }

    #[test]
    fn test_repository_connection_reuses_schema_sqlite_configuration() {
        assert_runtime_connection_reuses_schema_sqlite_configuration();
    }

    #[test]
    fn test_database_connection_reuses_schema_sqlite_configuration() {
        assert_runtime_connection_reuses_schema_sqlite_configuration();
    }
}
