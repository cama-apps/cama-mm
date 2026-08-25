//! Rust-owned SQLite schema initialization and migration authority.
//!
//! Production startup calls [`initialize_or_migrate`] before constructing any
//! repositories. The implementation embeds the canonical SQLite schema and the
//! ordered migration manifest, so schema initialization is entirely owned by
//! the Rust runtime. All schema reconciliation, one-time data backfills, and
//! migration ledger inserts that are pending at lock acquisition commit in a
//! single `BEGIN IMMEDIATE` transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;
use std::time::{Duration, Instant};

use cama_domain::game_date::yesterday_of;
use cama_domain::openskill::{
    CamaOpenSkillSystem, OpenSkillConfig, Player as OpenSkillPlayer, Rating as OpenSkillRating,
    WeightedPlayer, WinningTeam,
};
use cama_domain::rating::{
    CamaRatingSystem, LEGACY_STREAK_MULTIPLIER_PER_GAME, LEGACY_STREAK_THRESHOLD,
    NEW_PLAYER_MMR_DISCOUNT, STREAK_MULTIPLIER_PER_GAME, STREAK_THRESHOLD,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior, params};
use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;

use crate::{
    expected_migrations,
    json_numeric::{coerce_i64_like_python, number_f64},
};

/// Canonical schema produced by the current ordered migration set.
pub const CANONICAL_SCHEMA_SQL: &str = include_str!("../../../schema/canonical_schema.sql");

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Deployment settings that affect durable migration backfills.
///
/// These values historically came from the Python process environment. The
/// Rust migration path retains the same parsing defaults so historical rating
/// replays remain deterministic. Semantically invalid OpenSkill ranges still
/// fail when the replay constructs its model.
#[derive(Clone, Debug, PartialEq)]
pub struct MigrationSettings {
    pub streak_threshold: i64,
    pub new_player_mmr_discount: i32,
    pub openskill: OpenSkillConfig,
}

impl Default for MigrationSettings {
    fn default() -> Self {
        let openskill = OpenSkillConfig {
            streak_threshold: STREAK_THRESHOLD,
            streak_multiplier_per_game: STREAK_MULTIPLIER_PER_GAME,
            ..OpenSkillConfig::default()
        };
        Self {
            streak_threshold: STREAK_THRESHOLD,
            new_player_mmr_discount: NEW_PLAYER_MMR_DISCOUNT,
            openskill,
        }
    }
}

impl MigrationSettings {
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|name| env::var(name).ok())
    }

    #[must_use]
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        let streak_threshold =
            parse_env_or(&mut lookup, "STREAK_THRESHOLD", defaults.streak_threshold);
        let mut openskill = defaults.openskill;
        openskill.calibration_threshold = parse_env_or(
            &mut lookup,
            "OPENSKILL_CALIBRATION_SIGMA_THRESHOLD",
            openskill.calibration_threshold,
        );
        openskill.performance_strength = parse_env_or(
            &mut lookup,
            "OPENSKILL_PERFORMANCE_STRENGTH",
            openskill.performance_strength,
        );
        openskill.streak_threshold = streak_threshold;
        openskill.streak_multiplier_per_game = parse_env_or(
            &mut lookup,
            "STREAK_MULTIPLIER_PER_GAME",
            openskill.streak_multiplier_per_game,
        );
        openskill.sigma_decay_grace_period_days = parse_env_or(
            &mut lookup,
            "OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS",
            openskill.sigma_decay_grace_period_days,
        );
        openskill.sigma_decay_per_week = parse_env_or(
            &mut lookup,
            "OPENSKILL_SIGMA_DECAY_PER_WEEK",
            openskill.sigma_decay_per_week,
        );
        openskill.win_probability_temperature = parse_env_or(
            &mut lookup,
            "OPENSKILL_WIN_PROBABILITY_TEMPERATURE",
            openskill.win_probability_temperature,
        );
        Self {
            streak_threshold,
            new_player_mmr_discount: parse_env_or(
                &mut lookup,
                "NEW_PLAYER_MMR_DISCOUNT",
                defaults.new_player_mmr_discount,
            ),
            openskill,
        }
    }
}

fn parse_env_or<T: std::str::FromStr>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &str,
    default: T,
) -> T {
    lookup(name)
        .and_then(|value| value.trim().parse::<T>().ok())
        .unwrap_or(default)
}
const RETIRED_TABLES: &[&str] = &[
    "curses",
    "dig_boss_echoes_old",
    "mana_daily_losses",
    "mana_shop_items",
    "pending_matches_old",
    "prediction_bets",
    "protected_hero_purchases",
    "war_bets",
    "wheel_wars",
];
const RETIRED_INDEXES: &[&str] = &[
    "idx_dig_actions_guild_actor",
    "idx_dig_actions_guild_target",
];

/// Result of one startup initialization/migration pass.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MigrationReport {
    /// Ledger entries present when the immediate transaction acquired its lock.
    pub previously_applied: usize,
    /// Canonical migration names inserted by this pass.
    pub newly_applied: Vec<String>,
    /// Final-schema tables created because no historical table existed.
    pub created_tables: Vec<String>,
    /// Historical tables rebuilt while retaining rows and extra columns.
    pub rebuilt_tables: Vec<String>,
    /// Unknown/retired migration names deliberately retained in the ledger.
    pub historical_extra_migrations: Vec<String>,
}

impl MigrationReport {
    /// Whether the database was already at the current logical migration head.
    #[must_use]
    pub fn was_current(&self) -> bool {
        self.newly_applied.is_empty()
            && self.created_tables.is_empty()
            && self.rebuilt_tables.is_empty()
    }
}

/// Startup schema error.  Every variant raised after `BEGIN IMMEDIATE` causes
/// the complete pending batch (including DDL and ledger rows) to roll back.
#[derive(Debug, Error)]
pub enum SchemaMigrationError {
    #[error("SQLite schema migration failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("canonical schema has no CREATE TABLE statement for {0}")]
    MissingCanonicalTable(String),
    #[error(
        "cannot migrate {table}: populated historical rows have no value for required column {column}"
    )]
    MissingRequiredValue { table: String, column: String },
    #[error("invalid canonical CREATE TABLE SQL for {0}")]
    InvalidCanonicalTableSql(String),
    #[error("migration batch was deliberately interrupted after {0} schema changes")]
    InjectedFailure(usize),
    #[error("OpenSkill replay failed: {0}")]
    OpenSkillReplay(String),
    #[error("cannot enforce unique Valve match IDs: {0}")]
    DuplicateValveMatchIds(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ColumnInfo {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_position: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SchemaObject {
    name: String,
    table_name: String,
    sql: String,
}

#[derive(Default)]
struct ReconcileReport {
    created_tables: Vec<String>,
    rebuilt_tables: Vec<String>,
}

/// Create or upgrade a file-backed database for use by the Rust runtime.
///
/// The connection enforces the canonical runtime storage configuration: WAL
/// journal mode, a 5-second busy timeout, foreign-key enforcement disabled, and
/// an immediate writer lock around the complete pending migration batch.
pub fn initialize_or_migrate(
    path: impl AsRef<Path>,
) -> Result<MigrationReport, SchemaMigrationError> {
    initialize_or_migrate_with_settings(path, &MigrationSettings::from_env())
}

/// Create or upgrade a database using explicit deployment settings.
///
/// This is primarily useful for deterministic schema-initialization tests.
/// Production callers should use [`initialize_or_migrate`], which reads the
/// deployment's rating environment variables.
pub fn initialize_or_migrate_with_settings(
    path: impl AsRef<Path>,
    settings: &MigrationSettings,
) -> Result<MigrationReport, SchemaMigrationError> {
    let path = path.as_ref();
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match initialize_or_migrate_inner(path, settings, None) {
            Err(error) if sqlite_is_busy(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

fn sqlite_is_busy(error: &SchemaMigrationError) -> bool {
    matches!(
        error,
        SchemaMigrationError::Sqlite(rusqlite::Error::SqliteFailure(details, _))
            if matches!(details.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn initialize_or_migrate_inner(
    path: &Path,
    settings: &MigrationSettings,
    fail_after_schema_changes: Option<usize>,
) -> Result<MigrationReport, SchemaMigrationError> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name TEXT PRIMARY KEY,
            applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );",
    )?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = migration_names(&transaction)?;
    let expected: BTreeSet<String> = expected_migrations()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let pending: Vec<String> = expected_migrations()
        .into_iter()
        .filter(|name| !applied.contains(*name))
        .map(str::to_owned)
        .collect();

    recover_interrupted_legacy_rebuilds(&transaction)?;
    prepare_legacy_required_values(&transaction, &pending)?;
    let reconcile = reconcile_schema(&transaction, fail_after_schema_changes)?;
    baseline_historical_mafia_resolutions(&transaction)?;
    run_consolidated_backfills(&transaction, &pending, settings)?;
    for name in &pending {
        transaction.execute("INSERT INTO schema_migrations(name) VALUES (?1)", [name])?;
    }

    let historical_extra_migrations = applied.difference(&expected).cloned().collect::<Vec<_>>();
    transaction.commit()?;

    Ok(MigrationReport {
        previously_applied: applied.len(),
        newly_applied: pending,
        created_tables: reconcile.created_tables,
        rebuilt_tables: reconcile.rebuilt_tables,
        historical_extra_migrations,
    })
}

/// Python-era resolved Mafia rows have no Rust delivery receipt. Mark those
/// historical rows as already delivered once the nullable receipt columns are
/// initialized. A Rust settlement always writes `resolution_summary` in the same
/// transaction as the phase transition, so a resolved row with a non-null
/// summary and a NULL message ID remains eligible for deterministic Discord
/// replay.
fn baseline_historical_mafia_resolutions(
    transaction: &Transaction<'_>,
) -> Result<(), SchemaMigrationError> {
    if !table_exists(transaction, "mafia_games")? {
        return Ok(());
    }
    let columns = table_columns(transaction, "mafia_games")?;
    let has_delivery = columns
        .iter()
        .any(|column| column.name == "resolution_message_id");
    let has_summary = columns
        .iter()
        .any(|column| column.name == "resolution_summary");
    if has_delivery && has_summary {
        transaction.execute(
            "UPDATE mafia_games SET resolution_message_id=0
             WHERE phase='RESOLVED' AND resolution_message_id IS NULL
               AND resolution_summary IS NULL",
            [],
        )?;
    }
    Ok(())
}

fn canonical_connection() -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open_in_memory()?;
    connection.pragma_update(None, "foreign_keys", false)?;
    connection.execute_batch(CANONICAL_SCHEMA_SQL)?;
    Ok(connection)
}

fn reconcile_schema(
    transaction: &Transaction<'_>,
    fail_after_schema_changes: Option<usize>,
) -> Result<ReconcileReport, SchemaMigrationError> {
    let canonical = canonical_connection()?;
    let canonical_tables = schema_objects(&canonical, "table")?;
    let mut report = ReconcileReport::default();
    let mut changes = 0_usize;

    for retired in RETIRED_TABLES {
        if table_exists(transaction, retired)? {
            transaction.execute_batch(&format!("DROP TABLE {}", quote_identifier(retired)))?;
            changes += 1;
            maybe_fail(changes, fail_after_schema_changes)?;
        }
    }
    for retired in RETIRED_INDEXES {
        transaction.execute_batch(&format!(
            "DROP INDEX IF EXISTS {}",
            quote_identifier(retired)
        ))?;
    }

    for object in canonical_tables.values() {
        if object.name == "schema_migrations" {
            continue;
        }
        if !table_exists(transaction, &object.name)? {
            transaction.execute_batch(&object.sql)?;
            report.created_tables.push(object.name.clone());
            changes += 1;
            maybe_fail(changes, fail_after_schema_changes)?;
            continue;
        }

        let expected_columns = table_columns(&canonical, &object.name)?;
        let actual_columns = table_columns(transaction, &object.name)?;
        let actual_sql: String = transaction.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
            [&object.name],
            |row| row.get(0),
        )?;
        let same_column_count = expected_columns.len() == actual_columns.len();
        if table_needs_rebuild(&expected_columns, &actual_columns)
            || (same_column_count && normalize_sql(&actual_sql) != normalize_sql(&object.sql))
        {
            rebuild_table(
                transaction,
                &object.name,
                &object.sql,
                &expected_columns,
                &actual_columns,
            )?;
            report.rebuilt_tables.push(object.name.clone());
            changes += 1;
            maybe_fail(changes, fail_after_schema_changes)?;
        }
    }

    reconcile_schema_objects(transaction, &canonical, "index")?;
    reconcile_schema_objects(transaction, &canonical, "trigger")?;
    report.created_tables.sort();
    report.rebuilt_tables.sort();
    Ok(report)
}

fn maybe_fail(
    changes: usize,
    fail_after_schema_changes: Option<usize>,
) -> Result<(), SchemaMigrationError> {
    if fail_after_schema_changes == Some(changes) {
        return Err(SchemaMigrationError::InjectedFailure(changes));
    }
    Ok(())
}

fn table_needs_rebuild(expected: &[ColumnInfo], actual: &[ColumnInfo]) -> bool {
    let actual_by_name: BTreeMap<&str, &ColumnInfo> = actual
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect();
    expected.iter().any(|expected_column| {
        actual_by_name
            .get(expected_column.name.as_str())
            .is_none_or(|actual_column| !column_contract_matches(expected_column, actual_column))
    })
}

fn column_contract_matches(expected: &ColumnInfo, actual: &ColumnInfo) -> bool {
    expected
        .declared_type
        .eq_ignore_ascii_case(&actual.declared_type)
        && expected.not_null == actual.not_null
        && normalized_default(expected.default_value.as_deref())
            == normalized_default(actual.default_value.as_deref())
        && expected.primary_key_position == actual.primary_key_position
}

fn normalized_default(value: Option<&str>) -> Option<String> {
    value.map(|raw| {
        let mut normalized = raw.trim();
        while normalized.starts_with('(') && normalized.ends_with(')') && normalized.len() > 1 {
            normalized = &normalized[1..normalized.len() - 1];
        }
        normalized
            .split_whitespace()
            .collect::<String>()
            .to_lowercase()
    })
}

fn rebuild_table(
    transaction: &Transaction<'_>,
    table: &str,
    canonical_sql: &str,
    expected_columns: &[ColumnInfo],
    actual_columns: &[ColumnInfo],
) -> Result<(), SchemaMigrationError> {
    let temporary = format!("__cama_migrate_{table}");
    transaction.execute_batch(&format!(
        "DROP TABLE IF EXISTS {}",
        quote_identifier(&temporary)
    ))?;
    let body_start = canonical_sql
        .find('(')
        .ok_or_else(|| SchemaMigrationError::InvalidCanonicalTableSql(table.to_owned()))?;
    transaction.execute_batch(&format!(
        "CREATE TABLE {} {}",
        quote_identifier(&temporary),
        &canonical_sql[body_start..]
    ))?;

    let expected_names: BTreeSet<&str> = expected_columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    for extra in actual_columns
        .iter()
        .filter(|column| !expected_names.contains(column.name.as_str()))
    {
        let declared_type = safe_extra_declared_type(&extra.declared_type);
        transaction.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN {}{}",
            quote_identifier(&temporary),
            quote_identifier(&extra.name),
            declared_type
        ))?;
    }

    let row_count: i64 = transaction.query_row(
        &format!("SELECT COUNT(*) FROM {}", quote_identifier(table)),
        [],
        |row| row.get(0),
    )?;
    let expected_by_name: BTreeMap<&str, &ColumnInfo> = expected_columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect();
    let actual_names: BTreeSet<&str> = actual_columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();

    if row_count > 0 {
        for expected in expected_columns {
            if !actual_names.contains(expected.name.as_str())
                && expected.not_null
                && expected.default_value.is_none()
                && expected.primary_key_position == 0
            {
                return Err(SchemaMigrationError::MissingRequiredValue {
                    table: table.to_owned(),
                    column: expected.name.clone(),
                });
            }
        }

        let mut targets = Vec::new();
        let mut expressions = Vec::new();
        for actual in actual_columns {
            let target = expected_by_name
                .get(actual.name.as_str())
                .copied()
                .unwrap_or(actual);
            targets.push(quote_identifier(&target.name));
            let source = quote_identifier(&actual.name);
            if target.not_null {
                if let Some(default_value) = &target.default_value {
                    expressions.push(format!("COALESCE({source}, {default_value})"));
                } else {
                    expressions.push(source);
                }
            } else {
                expressions.push(source);
            }
        }
        if targets.is_empty() {
            return Err(SchemaMigrationError::MissingCanonicalTable(
                table.to_owned(),
            ));
        }
        transaction.execute_batch(&format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            quote_identifier(&temporary),
            targets.join(", "),
            expressions.join(", "),
            quote_identifier(table)
        ))?;
    }

    transaction.execute_batch(&format!(
        "DROP TABLE {}; ALTER TABLE {} RENAME TO {}",
        quote_identifier(table),
        quote_identifier(&temporary),
        quote_identifier(table)
    ))?;
    Ok(())
}

fn safe_extra_declared_type(declared_type: &str) -> String {
    if declared_type.is_empty() {
        return String::new();
    }
    if declared_type.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character.is_ascii_whitespace()
            || matches!(character, '_' | '(' | ')' | ',')
    }) {
        format!(" {declared_type}")
    } else {
        String::new()
    }
}

fn reconcile_schema_objects(
    transaction: &Transaction<'_>,
    canonical: &Connection,
    object_type: &str,
) -> Result<(), SchemaMigrationError> {
    for object in schema_objects(canonical, object_type)?.values() {
        let existing_sql = transaction
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type=?1 AND name=?2",
                params![object_type, object.name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing_sql
            .as_deref()
            .is_some_and(|sql| normalize_sql(sql) == normalize_sql(&object.sql))
        {
            continue;
        }
        if existing_sql.is_some() {
            transaction.execute_batch(&format!(
                "DROP {} {}",
                object_type.to_ascii_uppercase(),
                quote_identifier(&object.name)
            ))?;
        }
        transaction.execute_batch(&object.sql)?;
    }
    Ok(())
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != '"')
        .flat_map(char::to_lowercase)
        .collect()
}

fn schema_objects(
    connection: &Connection,
    object_type: &str,
) -> Result<BTreeMap<String, SchemaObject>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name, tbl_name, sql
         FROM sqlite_master
         WHERE type=?1 AND sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    statement
        .query_map([object_type], |row| {
            Ok(SchemaObject {
                name: row.get(0)?,
                table_name: row.get(1)?,
                sql: row.get(2)?,
            })
        })?
        .map(|result| result.map(|object| (object.name.clone(), object)))
        .collect()
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<ColumnInfo>, rusqlite::Error> {
    let mut statement =
        connection.prepare(&format!("PRAGMA table_info({})", quote_identifier(table)))?;
    statement
        .query_map([], |row| {
            Ok(ColumnInfo {
                name: row.get(1)?,
                declared_type: row.get::<_, String>(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_value: row.get(4)?,
                primary_key_position: row.get(5)?,
            })
        })?
        .collect()
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
        )",
        [table],
        |row| row.get::<_, i64>(0).map(|value| value != 0),
    )
}

fn migration_names(connection: &Connection) -> Result<BTreeSet<String>, rusqlite::Error> {
    let mut statement = connection.prepare("SELECT name FROM schema_migrations")?;
    statement.query_map([], |row| row.get(0))?.collect()
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn recover_interrupted_legacy_rebuilds(
    transaction: &Transaction<'_>,
) -> Result<(), SchemaMigrationError> {
    // These names were used by historical rebuilds. A process can therefore
    // arrive with only the source table, or with both the source and a partly
    // populated destination. Merge before canonical reconciliation so neither
    // side is silently discarded.
    if table_exists(transaction, "pending_matches_old")? {
        if table_exists(transaction, "pending_matches")? {
            let live_is_current = table_columns(transaction, "pending_matches")?
                .iter()
                .any(|column| column.name == "pending_match_id");
            if live_is_current {
                transaction.execute_batch(
                    "INSERT INTO pending_matches(guild_id, payload, updated_at)
                     SELECT old.guild_id, old.payload, old.updated_at
                     FROM pending_matches_old AS old
                     WHERE NOT EXISTS (
                       SELECT 1 FROM pending_matches AS live
                       WHERE live.guild_id=old.guild_id
                         AND live.payload=old.payload
                         AND live.updated_at IS old.updated_at
                     );
                     DROP TABLE pending_matches_old;",
                )?;
            } else {
                transaction.execute_batch(
                    "INSERT OR IGNORE INTO pending_matches_old(guild_id, payload, updated_at)
                     SELECT guild_id, payload, updated_at FROM pending_matches;
                     DROP TABLE pending_matches;
                     ALTER TABLE pending_matches_old RENAME TO pending_matches;",
                )?;
            }
        } else {
            transaction
                .execute_batch("ALTER TABLE pending_matches_old RENAME TO pending_matches")?;
        }
    }

    if table_exists(transaction, "dig_boss_echoes_old")? {
        if table_exists(transaction, "dig_boss_echoes")? {
            let live_is_current = table_columns(transaction, "dig_boss_echoes")?
                .iter()
                .any(|column| column.name == "boss_id");
            if live_is_current {
                transaction.execute_batch(
                    "INSERT OR REPLACE INTO dig_boss_echoes(
                         guild_id, boss_id, depth, killer_discord_id, weakened_until
                     )
                     SELECT guild_id,
                            CASE depth
                              WHEN 25 THEN 'grothak'
                              WHEN 50 THEN 'crystalia'
                              WHEN 75 THEN 'magmus_rex'
                              WHEN 100 THEN 'void_warden'
                              WHEN 150 THEN 'sporeling_sovereign'
                              WHEN 200 THEN 'chronofrost'
                              WHEN 275 THEN 'nameless_depth'
                              ELSE printf('depth_%d', depth)
                            END,
                            depth, killer_discord_id, weakened_until
                     FROM dig_boss_echoes_old;
                     DROP TABLE dig_boss_echoes_old;",
                )?;
            } else {
                transaction.execute_batch(
                    "INSERT OR IGNORE INTO dig_boss_echoes_old(
                         guild_id, depth, killer_discord_id, weakened_until
                     )
                     SELECT guild_id, depth, killer_discord_id, weakened_until
                     FROM dig_boss_echoes;
                     DROP TABLE dig_boss_echoes;
                     ALTER TABLE dig_boss_echoes_old RENAME TO dig_boss_echoes;",
                )?;
            }
        } else {
            transaction
                .execute_batch("ALTER TABLE dig_boss_echoes_old RENAME TO dig_boss_echoes")?;
        }
    }
    Ok(())
}

fn prepare_legacy_required_values(
    transaction: &Transaction<'_>,
    pending_migrations: &[String],
) -> Result<(), SchemaMigrationError> {
    if pending(
        pending_migrations,
        "create_match_discovery_attempts_and_unique_valve_ids",
    ) && table_exists(transaction, "matches")?
    {
        let columns = table_columns(transaction, "matches")?;
        if columns.iter().any(|column| column.name == "valve_match_id") {
            let guild_expression = if columns.iter().any(|column| column.name == "guild_id") {
                "guild_id"
            } else {
                "0"
            };
            let duplicate_sql = format!(
                "SELECT {guild_expression}, valve_match_id, GROUP_CONCAT(match_id), COUNT(*)
                   FROM matches
                  WHERE valve_match_id IS NOT NULL
                  GROUP BY {guild_expression}, valve_match_id
                 HAVING COUNT(*) > 1
                  ORDER BY {guild_expression}, valve_match_id
                  LIMIT 1"
            );
            let duplicate = transaction
                .query_row(&duplicate_sql, [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .optional()?;
            if let Some((guild_id, valve_match_id, match_ids, count)) = duplicate {
                return Err(SchemaMigrationError::DuplicateValveMatchIds(format!(
                    "guild {guild_id} has Valve match {valve_match_id} assigned to {count} internal matches ({match_ids})"
                )));
            }
        }
    }
    if pending(pending_migrations, "rekey_dig_boss_echoes_by_boss_id")
        && table_exists(transaction, "dig_boss_echoes")?
    {
        let columns = table_columns(transaction, "dig_boss_echoes")?;
        if !columns.iter().any(|column| column.name == "boss_id") {
            transaction.execute_batch(
                "ALTER TABLE dig_boss_echoes ADD COLUMN boss_id TEXT;
                 UPDATE dig_boss_echoes SET boss_id=CASE depth
                   WHEN 25 THEN 'grothak'
                   WHEN 50 THEN 'crystalia'
                   WHEN 75 THEN 'magmus_rex'
                   WHEN 100 THEN 'void_warden'
                   WHEN 150 THEN 'sporeling_sovereign'
                   WHEN 200 THEN 'chronofrost'
                   WHEN 275 THEN 'nameless_depth'
                   ELSE printf('depth_%d', depth)
                 END;",
            )?;
        }
    }
    if pending(pending_migrations, "add_pet_stable") && table_exists(transaction, "pets")? {
        let columns = table_columns(transaction, "pets")?;
        if !columns.iter().any(|column| column.name == "is_active") {
            transaction.execute_batch(
                "ALTER TABLE pets ADD COLUMN is_active INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE pets ADD COLUMN stabled_at INTEGER;
                 UPDATE pets
                 SET is_active=CASE WHEN died_at IS NULL THEN 1 ELSE 0 END,
                     stabled_at=NULL;",
            )?;
        }
    }
    Ok(())
}

fn pending(pending: &[String], migration: &str) -> bool {
    pending.iter().any(|candidate| candidate == migration)
}

fn run_consolidated_backfills(
    transaction: &Transaction<'_>,
    pending_migrations: &[String],
    settings: &MigrationSettings,
) -> Result<(), SchemaMigrationError> {
    if pending(pending_migrations, "add_survey_receipt_checked_attempt") {
        transaction.execute_batch(
            "UPDATE survey_recipients
             SET delivery_status='sending', claimed_at=COALESCE(claimed_at, updated_at)
             WHERE delivery_status='pending'
               AND attempt_count > receipt_checked_attempt;",
        )?;
    }
    if pending(pending_migrations, "add_streak_threshold_to_rating_history") {
        transaction.execute(
            "UPDATE rating_history SET streak_threshold=?1",
            [settings.streak_threshold],
        )?;
    }
    if pending(pending_migrations, "add_bet_payout_column") {
        transaction.execute_batch(
            "UPDATE bets
             SET payout=amount * COALESCE(leverage, 1) * 2
             WHERE match_id IS NOT NULL AND payout IS NULL
               AND bet_id IN (
                 SELECT b.bet_id FROM bets b JOIN matches m ON b.match_id=m.match_id
                 WHERE (m.winning_team=1 AND b.team_bet_on='radiant')
                    OR (m.winning_team=2 AND b.team_bet_on='dire')
               );",
        )?;
    }
    if pending(pending_migrations, "add_last_match_date_to_players") {
        transaction.execute_batch(
            "UPDATE players SET last_match_date=COALESCE(last_match_date, created_at)
             WHERE last_match_date IS NULL;",
        )?;
    }
    if pending(pending_migrations, "add_bankruptcy_count_column") {
        transaction.execute_batch(
            "UPDATE bankruptcy_state SET bankruptcy_count=1
             WHERE last_bankruptcy_at IS NOT NULL
               AND (bankruptcy_count IS NULL OR bankruptcy_count=0);",
        )?;
    }
    if pending(pending_migrations, "add_first_calibrated_at_to_players") {
        transaction.execute_batch(
            "UPDATE players
             SET first_calibrated_at=CAST(strftime('%s', created_at) AS INTEGER)
             WHERE glicko_rd IS NOT NULL AND glicko_rd <= 100.0
               AND first_calibrated_at IS NULL;",
        )?;
    }
    if pending(pending_migrations, "create_player_steam_ids_table") {
        transaction.execute_batch(
            "INSERT OR IGNORE INTO player_steam_ids(
                 discord_id, steam_id, is_primary, added_at
             )
             SELECT discord_id, steam_id, 1, CAST(strftime('%s','now') AS INTEGER)
             FROM players WHERE steam_id IS NOT NULL;",
        )?;
    }
    if pending(pending_migrations, "add_dota_streak_to_players") {
        backfill_dota_streak(transaction)?;
    }
    if pending(pending_migrations, "upgrade_boss_progress_json") {
        transform_boss_progress(transaction, BossProgressTransform::AddBossIds)?;
    }
    if pending(pending_migrations, "clear_active_boss_ids_for_pool_reroll") {
        transform_boss_progress(transaction, BossProgressTransform::ClearActiveBossIds)?;
    }
    if pending(pending_migrations, "create_dig_gear_system") {
        transaction.execute_batch(
            "INSERT INTO dig_gear(
                 discord_id, guild_id, slot, tier, durability,
                 equipped, acquired_at, source
             )
             SELECT discord_id, guild_id, 'weapon', pickaxe_tier, 20, 1,
                    COALESCE(last_dig_at, CAST(strftime('%s','now') AS INTEGER)),
                    'migration'
             FROM tunnels;",
        )?;
    }
    if pending(pending_migrations, "dig_boss_progress_persistent_hp") {
        transform_boss_progress(transaction, BossProgressTransform::WrapStatuses)?;
    }
    if pending(pending_migrations, "predictions_mini_split_v1") {
        transaction.execute_batch(
            "UPDATE prediction_positions
             SET yes_contracts=yes_contracts*10, no_contracts=no_contracts*10;
             UPDATE prediction_levels SET remaining_size=remaining_size*10;
             UPDATE prediction_trades SET contracts=contracts*10;
             INSERT OR IGNORE INTO prediction_fair_snapshots(
                 market_id, guild_id, snapshot_at, fair_pct, reason
             )
             SELECT prediction_id, guild_id,
                    COALESCE(last_refresh_at, CAST(strftime('%s','now') AS INTEGER)),
                    COALESCE(current_price, initial_fair), 'backfill'
             FROM predictions
             WHERE status='open'
               AND COALESCE(current_price, initial_fair) IS NOT NULL;
             INSERT OR IGNORE INTO app_kv(guild_id,key,value)
             SELECT DISTINCT p.guild_id, 'split_announced', '0'
             FROM prediction_trades t
             JOIN predictions p ON p.prediction_id=t.prediction_id;",
        )?;
    }
    if pending(
        pending_migrations,
        "predictions_fair_history_backfill_from_levels",
    ) {
        transaction.execute_batch(
            "INSERT INTO prediction_fair_snapshots(
                 market_id, guild_id, snapshot_at, fair_pct, reason
             )
             SELECT l.prediction_id, p.guild_id,
                    CAST(strftime('%s', date(l.posted_at,'unixepoch'),
                                  '+1 day','-1 second') AS INTEGER) AS eod_ts,
                    CASE
                      WHEN MIN(CASE WHEN l.side='yes_ask' THEN l.price END) IS NOT NULL
                       AND MAX(CASE WHEN l.side='yes_bid' THEN l.price END) IS NOT NULL
                      THEN (MIN(CASE WHEN l.side='yes_ask' THEN l.price END)
                          + MAX(CASE WHEN l.side='yes_bid' THEN l.price END))/2
                      WHEN MIN(CASE WHEN l.side='yes_ask' THEN l.price END) IS NOT NULL
                      THEN MIN(CASE WHEN l.side='yes_ask' THEN l.price END)
                      ELSE MAX(CASE WHEN l.side='yes_bid' THEN l.price END)
                    END,
                    'backfill_levels'
             FROM prediction_levels l
             JOIN predictions p ON p.prediction_id=l.prediction_id
             GROUP BY l.prediction_id, date(l.posted_at,'unixepoch')
             HAVING NOT EXISTS (
               SELECT 1 FROM prediction_fair_snapshots s
               WHERE s.market_id=l.prediction_id AND s.guild_id=p.guild_id
                 AND s.snapshot_at=eod_ts
             );",
        )?;
    }
    if pending(pending_migrations, "cap_soft_avoid_games_remaining") {
        transaction
            .execute_batch("UPDATE soft_avoids SET games_remaining=10 WHERE games_remaining>10;")?;
    }
    if pending(pending_migrations, "cap_package_deal_games_remaining") {
        transaction.execute_batch(
            "UPDATE package_deals SET games_remaining=10 WHERE games_remaining>10;",
        )?;
    }
    if pending(
        pending_migrations,
        "normalize_null_guild_id_pairings_and_neon",
    ) {
        transaction.execute_batch(
            "UPDATE player_pairings SET guild_id=0 WHERE guild_id IS NULL;
             UPDATE neon_events SET guild_id=0 WHERE guild_id IS NULL;",
        )?;
    }
    if pending(
        pending_migrations,
        "reconcile_post_prestige_boss_stat_points",
    ) {
        reconcile_boss_stat_points(transaction, false)?;
    }
    if pending(
        pending_migrations,
        "reconcile_cumulative_prestige_boss_stat_points",
    ) {
        reconcile_boss_stat_points(transaction, true)?;
    }
    if pending(pending_migrations, "backfill_missing_dig_weapon_gear") {
        transaction.execute_batch(
            "INSERT INTO dig_gear(
                 discord_id, guild_id, slot, tier, durability,
                 equipped, acquired_at, source
             )
             SELECT t.discord_id, COALESCE(t.guild_id,0), 'weapon',
                    COALESCE(t.pickaxe_tier,0), 20, 1,
                    COALESCE(t.last_dig_at,t.created_at,
                             CAST(strftime('%s','now') AS INTEGER)),
                    'missing_weapon_backfill'
             FROM tunnels t
             WHERE NOT EXISTS (
               SELECT 1 FROM dig_gear g
               WHERE g.discord_id=t.discord_id
                 AND g.guild_id=COALESCE(t.guild_id,0)
                 AND g.slot='weapon'
             );",
        )?;
    }
    if pending(pending_migrations, "backfill_overgrowth_charges_remaining") {
        backfill_overgrowth_charges(transaction)?;
    }
    if pending(
        pending_migrations,
        "renumber_pickaxe_tier_for_stormrend_insert",
    ) {
        transaction.execute_batch(
            "UPDATE tunnels SET pickaxe_tier=7 WHERE pickaxe_tier=6;
             UPDATE tunnels SET pickaxe_tier=6 WHERE pickaxe_tier=5;
             UPDATE dig_gear SET tier=7 WHERE tier=6;
             UPDATE dig_gear SET tier=6 WHERE tier=5;",
        )?;
    }
    if pending(pending_migrations, "relic_loadout_cap_and_streak") {
        transaction.execute_batch(
            "UPDATE tunnels SET relic_trim_notice=1
             WHERE (
               SELECT COUNT(*) FROM dig_artifacts d
               WHERE d.is_relic=1 AND d.equipped=1
                 AND d.discord_id=tunnels.discord_id
                 AND d.guild_id=tunnels.guild_id
             )>6;
             UPDATE dig_artifacts SET equipped=0
             WHERE id IN (
               SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (
                   PARTITION BY discord_id,guild_id ORDER BY id DESC
                 ) AS rn
                 FROM dig_artifacts WHERE is_relic=1 AND equipped=1
               ) WHERE rn>6
             );",
        )?;
    }
    if pending(pending_migrations, "create_economy_ledger_tables")
        || pending(
            pending_migrations,
            "backfill_economy_ledger_opening_balances",
        )
    {
        backfill_economy_ledger(transaction)?;
    }
    if pending(pending_migrations, "schedule_unresolved_duel_reminders") {
        transaction.execute_batch(
            "UPDATE duel_challenges
             SET next_reminder_at=CAST(strftime('%s','now') AS INTEGER)+86400
             WHERE status='accepted' AND next_reminder_at IS NULL;",
        )?;
    }
    if pending(pending_migrations, "create_match_bans_for_scout") {
        backfill_match_bans(transaction)?;
    }
    if pending(pending_migrations, "recompute_symmetric_glicko_predictions") {
        backfill_glicko_probabilities(transaction)?;
    }
    if pending(pending_migrations, "create_wrapped_enrichment_facts") {
        backfill_wrapped_enrichment(transaction)?;
    }
    if pending(
        pending_migrations,
        "add_announced_at_to_economy_daily_events",
    ) {
        transaction.execute_batch(
            "UPDATE economy_daily_events SET announced_at=created_at
             WHERE announced_at IS NULL;",
        )?;
    }
    if pending(pending_migrations, "add_consumed_today_to_player_mana") {
        transaction.execute_batch(
            "UPDATE player_mana SET consumed_today=0 WHERE consumed_today IS NULL;",
        )?;
    }
    if pending(
        pending_migrations,
        "add_white_shield_remaining_to_player_mana",
    ) {
        transaction.execute_batch(
            "UPDATE player_mana SET white_shield_remaining=25
             WHERE current_land='Plains' AND consumed_today=0
               AND white_shield_remaining=0;",
        )?;
    }
    if pending(pending_migrations, "add_mafia_weekly_game_columns") {
        transaction.execute_batch(
            "UPDATE mafia_games SET phase_started_at=started_at
             WHERE phase_started_at IS NULL;",
        )?;
    }
    if pending(pending_migrations, "rebuild_mafia_actions_per_night") {
        transaction
            .execute_batch("UPDATE mafia_actions SET day_number=1 WHERE day_number IS NULL;")?;
    }
    if pending(pending_migrations, "add_pet_refund_announcement_state") {
        transaction.execute_batch(
            "UPDATE pet_refund_windows SET announced_at=paid_at
             WHERE announcement_payload IS NULL AND announced_at IS NULL;",
        )?;
    }
    if pending(pending_migrations, "add_pet_dig_work") {
        transaction.execute_batch(
            "UPDATE pets SET dig_work_at=CAST(strftime('%s','now') AS INTEGER)
             WHERE dig_work_at=0;",
        )?;
    }
    if pending(pending_migrations, "add_pet_hatch_roll_state") {
        transaction.execute_batch(
            "UPDATE pets SET egg_tier=CASE
                 WHEN adopt_fee>100 THEN 'gilded' ELSE 'standard' END;
             UPDATE pets SET species='unhatched'
             WHERE died_at IS NULL
               AND hatched_at>CAST(strftime('%s','now') AS INTEGER);",
        )?;
    }
    if pending(pending_migrations, "add_pet_evolution") {
        transaction.execute_batch(
            "UPDATE pets
             SET evolution_started_at=COALESCE(
                   evolution_started_at,
                   CASE
                     WHEN hatched_at>CAST(strftime('%s','now') AS INTEGER)
                     THEN hatched_at ELSE CAST(strftime('%s','now') AS INTEGER)
                   END
                 ),
                 evolution_due_at=COALESCE(
                   evolution_due_at,
                   COALESCE(
                     evolution_started_at,
                     CASE
                       WHEN hatched_at>CAST(strftime('%s','now') AS INTEGER)
                       THEN hatched_at ELSE CAST(strftime('%s','now') AS INTEGER)
                     END
                   )+604800
                 )
             WHERE died_at IS NULL
               AND (evolution_started_at IS NULL OR evolution_due_at IS NULL);",
        )?;
    }
    if pending(pending_migrations, "add_pet_stable") {
        transaction.execute_batch(
            "UPDATE pets
             SET is_active=CASE WHEN died_at IS NULL THEN 1 ELSE 0 END,
                 stabled_at=NULL;",
        )?;
    }
    if pending(
        pending_migrations,
        "clear_dig_active_duels_for_retired_timed_mechanics",
    ) {
        transaction.execute_batch(
            "DELETE FROM dig_active_duels
             WHERE mechanic_id IN (
               'pinnacle_arithmetic_challenge','pinnacle_riddle_challenge'
             );",
        )?;
    }
    if pending(pending_migrations, "add_survey_retry_requested") {
        transaction.execute_batch(
            "UPDATE survey_recipients SET retry_requested=0
             WHERE retry_requested IS NULL;",
        )?;
    }
    let openskill_replay_count = [
        "openskill_v3_durable_native_replay",
        "openskill_v4_streak_replay",
        "openskill_v5_gain_multiplier_replay",
    ]
    .into_iter()
    .filter(|migration| pending(pending_migrations, migration))
    .count();
    for _ in 0..openskill_replay_count {
        replay_openskill(transaction, settings)?;
    }
    Ok(())
}

fn backfill_dota_streak(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT mp.discord_id, COALESCE(mp.guild_id,0),
                    date(m.match_date,'-12 hours') AS game_date
             FROM match_participants mp
             JOIN matches m ON m.match_id=mp.match_id
             ORDER BY mp.discord_id, mp.guild_id, m.match_date DESC",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut dates_by_player: BTreeMap<(i64, i64), Vec<String>> = BTreeMap::new();
    for (discord_id, guild_id, game_date) in rows {
        let Some(game_date) = game_date else {
            continue;
        };
        let dates = dates_by_player.entry((discord_id, guild_id)).or_default();
        if dates.last() != Some(&game_date) {
            dates.push(game_date);
        }
    }
    for ((discord_id, guild_id), dates) in dates_by_player {
        let Some(last_played) = dates.first() else {
            continue;
        };
        let mut streak = 1_i64;
        let mut previous = last_played.clone();
        for date in dates.iter().skip(1) {
            let Ok(yesterday) = yesterday_of(&previous) else {
                break;
            };
            if *date != yesterday {
                break;
            }
            streak += 1;
            previous.clone_from(date);
        }
        transaction.execute(
            "UPDATE players
             SET dota_streak_days=?1, dota_last_played_date=?2
             WHERE discord_id=?3 AND guild_id=?4",
            params![streak, last_played, discord_id, guild_id],
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum BossProgressTransform {
    AddBossIds,
    WrapStatuses,
    ClearActiveBossIds,
}

fn transform_boss_progress(
    transaction: &Transaction<'_>,
    transform: BossProgressTransform,
) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT discord_id, guild_id, boss_progress
             FROM tunnels
             WHERE boss_progress IS NOT NULL AND boss_progress!=''",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (discord_id, guild_id, raw) in rows {
        let Ok(JsonValue::Object(mut progress)) = serde_json::from_str(&raw) else {
            continue;
        };
        let mut changed = false;
        for (depth, value) in &mut progress {
            match transform {
                BossProgressTransform::AddBossIds => {
                    let JsonValue::String(status) = value else {
                        continue;
                    };
                    let boss_id = if status == "active" {
                        ""
                    } else {
                        depth
                            .parse::<i64>()
                            .ok()
                            .and_then(legacy_boss_id)
                            .unwrap_or("")
                    };
                    let mut upgraded = JsonMap::new();
                    upgraded.insert("boss_id".to_owned(), JsonValue::String(boss_id.to_owned()));
                    upgraded.insert("status".to_owned(), JsonValue::String(status.clone()));
                    *value = JsonValue::Object(upgraded);
                    changed = true;
                }
                BossProgressTransform::WrapStatuses => {
                    let JsonValue::String(status) = value else {
                        continue;
                    };
                    let mut upgraded = JsonMap::new();
                    upgraded.insert("status".to_owned(), JsonValue::String(status.clone()));
                    *value = JsonValue::Object(upgraded);
                    changed = true;
                }
                BossProgressTransform::ClearActiveBossIds => {
                    let JsonValue::Object(fields) = value else {
                        continue;
                    };
                    let active = fields.get("status").and_then(JsonValue::as_str) == Some("active");
                    let locked = fields
                        .get("boss_id")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|boss_id| !boss_id.is_empty());
                    if active && locked {
                        fields.insert("boss_id".to_owned(), JsonValue::String(String::new()));
                        changed = true;
                    }
                }
            }
        }
        if changed {
            transaction.execute(
                "UPDATE tunnels SET boss_progress=?1
                 WHERE discord_id=?2 AND guild_id=?3",
                params![
                    JsonValue::Object(progress).to_string(),
                    discord_id,
                    guild_id
                ],
            )?;
        }
    }
    Ok(())
}

const fn legacy_boss_id(depth: i64) -> Option<&'static str> {
    match depth {
        25 => Some("grothak"),
        50 => Some("crystalia"),
        75 => Some("magmus_rex"),
        100 => Some("void_warden"),
        150 => Some("sporeling_sovereign"),
        200 => Some("chronofrost"),
        275 => Some("nameless_depth"),
        _ => None,
    }
}

fn boss_defeats(progress: &JsonValue) -> BTreeSet<i64> {
    const BOUNDARIES: [i64; 7] = [25, 50, 75, 100, 150, 200, 275];
    let Some(entries) = progress.as_object() else {
        return BTreeSet::new();
    };
    BOUNDARIES
        .into_iter()
        .filter(|boundary| {
            let value = entries.get(&boundary.to_string());
            let status = value.and_then(|entry| match entry {
                JsonValue::Object(fields) => fields.get("status").and_then(JsonValue::as_str),
                JsonValue::String(status) => Some(status.as_str()),
                _ => None,
            });
            status == Some("defeated")
        })
        .collect()
}

fn current_awards(awards: &JsonValue, prestige_level: i64) -> BTreeSet<i64> {
    let Some(fields) = awards.as_object() else {
        return BTreeSet::new();
    };
    if fields
        .get("prestige_level")
        .and_then(json_coerce_i64)
        .unwrap_or_default()
        != prestige_level
    {
        return BTreeSet::new();
    }
    fields
        .get("awards")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(json_coerce_i64)
        .collect()
}

fn awards_json(prestige_level: i64, awards: &BTreeSet<i64>) -> String {
    let mut fields = JsonMap::new();
    fields.insert("prestige_level".to_owned(), prestige_level.into());
    fields.insert(
        "awards".to_owned(),
        JsonValue::Array(awards.iter().copied().map(JsonValue::from).collect()),
    );
    JsonValue::Object(fields).to_string()
}

fn reconcile_boss_stat_points(
    transaction: &Transaction<'_>,
    cumulative: bool,
) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT discord_id, guild_id, COALESCE(prestige_level,0),
                    COALESCE(stat_points,5), COALESCE(stat_boss_awards,'[]'),
                    COALESCE(boss_progress,'{}')
             FROM tunnels WHERE COALESCE(prestige_level,0)>0",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (discord_id, guild_id, prestige, current_points, raw_awards, raw_progress) in rows {
        let progress: JsonValue = serde_json::from_str(&raw_progress).unwrap_or(JsonValue::Null);
        let defeated = boss_defeats(&progress);
        if defeated.is_empty() && !cumulative {
            continue;
        }
        let decoded_awards: JsonValue =
            serde_json::from_str(&raw_awards).unwrap_or(JsonValue::Null);
        let awarded = current_awards(&decoded_awards, prestige);
        let updated_awards = awarded.union(&defeated).copied().collect::<BTreeSet<_>>();
        let expected_points = 5 + prestige * 7 + i64::try_from(defeated.len()).unwrap_or(0);

        if cumulative {
            if current_points >= expected_points {
                continue;
            }
            transaction.execute(
                "UPDATE tunnels SET stat_points=?1, stat_boss_awards=?2
                 WHERE discord_id=?3 AND guild_id=?4",
                params![
                    expected_points,
                    awards_json(prestige, &updated_awards),
                    discord_id,
                    guild_id
                ],
            )?;
            continue;
        }

        let missing_count = defeated.difference(&awarded).count();
        if missing_count == 0 {
            continue;
        }
        let next_points = if current_points >= expected_points {
            current_points
        } else {
            current_points.max(5) + i64::try_from(missing_count).unwrap_or(0)
        };
        transaction.execute(
            "UPDATE tunnels SET stat_points=?1, stat_boss_awards=?2
             WHERE discord_id=?3 AND guild_id=?4",
            params![
                next_points,
                awards_json(prestige, &updated_awards),
                discord_id,
                guild_id
            ],
        )?;
    }
    Ok(())
}

fn backfill_overgrowth_charges(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT id, data FROM manashop_buffs
             WHERE buff_type='overgrowth' AND triggered=0
               AND expires_at>CAST(strftime('%s','now') AS INTEGER)",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (buff_id, raw) in rows {
        let mut data = raw
            .as_deref()
            .and_then(|value| serde_json::from_str::<JsonValue>(value).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if data.contains_key("charges_remaining") {
            continue;
        }
        data.insert("charges_remaining".to_owned(), 10.into());
        transaction.execute(
            "UPDATE manashop_buffs SET data=?1 WHERE id=?2",
            params![JsonValue::Object(data).to_string(), buff_id],
        )?;
    }
    Ok(())
}

fn backfill_economy_ledger(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "WITH existing AS (
           SELECT guild_id,account_id,COALESCE(SUM(delta),0) AS logged_delta
           FROM economy_ledger_entries WHERE account_type='player'
           GROUP BY guild_id,account_id
         ), openings AS (
           SELECT COALESCE(p.guild_id,0) AS guild_id,p.discord_id AS account_id,
                  COALESCE(p.jopacoin_balance,0)-COALESCE(e.logged_delta,0) AS opening_balance
           FROM players p LEFT JOIN existing e
             ON e.guild_id=COALESCE(p.guild_id,0) AND e.account_id=p.discord_id
           WHERE NOT EXISTS (
             SELECT 1 FROM economy_ledger_entries le
             WHERE le.guild_id=COALESCE(p.guild_id,0)
               AND le.account_type='player' AND le.account_id=p.discord_id
               AND le.source='ledger_backfill'
           )
         )
         INSERT INTO economy_ledger_entries(
           guild_id,account_type,account_id,delta,balance_before,balance_after,source,reason
         )
         SELECT guild_id,'player',account_id,opening_balance,0,opening_balance,
                'ledger_backfill','opening balance at ledger creation'
         FROM openings WHERE opening_balance!=0;
         WITH existing AS (
           SELECT guild_id,account_id,COALESCE(SUM(delta),0) AS logged_delta
           FROM economy_ledger_entries WHERE account_type='nonprofit'
           GROUP BY guild_id,account_id
         ), openings AS (
           SELECT COALESCE(n.guild_id,0) AS guild_id,COALESCE(n.guild_id,0) AS account_id,
                  COALESCE(n.total_collected,0)-COALESCE(e.logged_delta,0) AS opening_balance
           FROM nonprofit_fund n LEFT JOIN existing e
             ON e.guild_id=COALESCE(n.guild_id,0)
            AND e.account_id=COALESCE(n.guild_id,0)
           WHERE NOT EXISTS (
             SELECT 1 FROM economy_ledger_entries le
             WHERE le.guild_id=COALESCE(n.guild_id,0)
               AND le.account_type='nonprofit'
               AND le.account_id=COALESCE(n.guild_id,0)
               AND le.source='ledger_backfill'
           )
         )
         INSERT INTO economy_ledger_entries(
           guild_id,account_type,account_id,delta,balance_before,balance_after,source,reason
         )
         SELECT guild_id,'nonprofit',account_id,opening_balance,0,opening_balance,
                'ledger_backfill','opening nonprofit fund at ledger creation'
         FROM openings WHERE opening_balance!=0;",
    )?;
    Ok(())
}

fn backfill_glicko_probabilities(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT match_id,radiant_rating,radiant_rd,dire_rating,dire_rd
             FROM match_predictions
             WHERE radiant_rating IS NOT NULL AND radiant_rd IS NOT NULL
               AND dire_rating IS NOT NULL AND dire_rd IS NOT NULL",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (match_id, radiant_rating, radiant_rd, dire_rating, dire_rd) in rows {
        let probability = CamaRatingSystem::predict_win_probability(
            radiant_rating,
            radiant_rd,
            dire_rating,
            dire_rd,
        );
        transaction.execute(
            "UPDATE match_predictions SET expected_radiant_win_prob=?1 WHERE match_id=?2",
            params![probability, match_id],
        )?;
        transaction.execute(
            "UPDATE rating_history
             SET expected_team_win_prob=CASE team_number
               WHEN 1 THEN ?1 WHEN 2 THEN 1.0-?1 ELSE expected_team_win_prob END
             WHERE match_id=?2",
            params![probability, match_id],
        )?;
    }
    Ok(())
}

fn backfill_match_bans(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT match_id,enrichment_data FROM matches WHERE enrichment_data IS NOT NULL",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (match_id, raw) in rows {
        let Ok(payload) = serde_json::from_str::<JsonValue>(&raw) else {
            continue;
        };
        let Some(entries) = payload.get("picks_bans").and_then(JsonValue::as_array) else {
            continue;
        };
        for (ban_index, entry) in entries.iter().enumerate() {
            let Some(fields) = entry.as_object() else {
                continue;
            };
            let is_ban = fields.get("is_pick").is_some_and(json_false_or_zero);
            if !is_ban {
                continue;
            }
            let Some(team) = fields.get("team").and_then(json_ban_integer) else {
                continue;
            };
            let Some(hero_id) = fields.get("hero_id").and_then(json_ban_integer) else {
                continue;
            };
            if !matches!(team, 0 | 1) || hero_id <= 0 {
                continue;
            }
            transaction.execute(
                "INSERT OR REPLACE INTO match_bans(match_id,ban_index,team,hero_id)
                 VALUES (?1,?2,?3,?4)",
                params![
                    match_id,
                    i64::try_from(ban_index).unwrap_or(i64::MAX),
                    team,
                    hero_id
                ],
            )?;
        }
    }
    Ok(())
}

fn json_coerce_i64(value: &JsonValue) -> Option<i64> {
    match value {
        JsonValue::Number(number) => number.as_i64(),
        JsonValue::String(number) => number.parse().ok(),
        JsonValue::Bool(_) | JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

fn json_false_or_zero(value: &JsonValue) -> bool {
    matches!(value, JsonValue::Bool(false))
        || matches!(value, JsonValue::Number(number) if number.as_f64() == Some(0.0))
}

fn json_ban_integer(value: &JsonValue) -> Option<i64> {
    (!value.is_boolean())
        .then(|| coerce_i64_like_python(value))
        .flatten()
}

fn json_number_f64(value: Option<&JsonValue>) -> Option<f64> {
    number_f64(value)
}

fn json_number_i64(value: Option<&JsonValue>) -> Option<i64> {
    value.and_then(|number| match number {
        JsonValue::Number(number) => number.as_i64(),
        _ => None,
    })
}

fn backfill_wrapped_enrichment(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM wrapped_enrichment_facts", [])?;
    let matches = {
        let mut statement = transaction.prepare(
            "SELECT match_id,guild_id,enrichment_data
             FROM matches WHERE enrichment_data IS NOT NULL ORDER BY match_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (match_id, guild_id, raw) in matches {
        let Ok(JsonValue::Object(payload)) = serde_json::from_str::<JsonValue>(&raw) else {
            continue;
        };
        let participant_rows = {
            let mut statement = transaction.prepare(
                "SELECT mp.discord_id,psi.steam_id,p.steam_id
                 FROM match_participants mp
                 LEFT JOIN player_steam_ids psi ON psi.discord_id=mp.discord_id
                 LEFT JOIN players p
                   ON p.discord_id=mp.discord_id AND p.guild_id=mp.guild_id
                 WHERE mp.match_id=?1 AND mp.guild_id=?2
                 ORDER BY mp.discord_id,psi.is_primary DESC,psi.added_at ASC",
            )?;
            statement
                .query_map(params![match_id, guild_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut steam_ids: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
        let mut legacy_ids = BTreeMap::new();
        for (discord_id, junction, legacy) in participant_rows {
            let ids = steam_ids.entry(discord_id).or_default();
            if let Some(junction) = junction {
                ids.insert(junction);
            }
            if let Some(legacy) = legacy {
                legacy_ids.insert(discord_id, legacy);
            }
        }
        for (discord_id, mut ids) in steam_ids {
            if ids.is_empty()
                && let Some(legacy) = legacy_ids.get(&discord_id)
            {
                ids.insert(*legacy);
            }
            let player = payload
                .get("players")
                .and_then(JsonValue::as_array)
                .into_iter()
                .flatten()
                .filter_map(JsonValue::as_object)
                .find(|candidate| {
                    candidate
                        .get("account_id")
                        .and_then(JsonValue::as_i64)
                        .is_some_and(|account_id| ids.contains(&account_id))
                });
            let rapier_count = player
                .and_then(|fields| fields.get("purchase_log"))
                .and_then(JsonValue::as_array)
                .map_or(0_i64, |purchases| {
                    i64::try_from(
                        purchases
                            .iter()
                            .filter_map(JsonValue::as_object)
                            .filter(|item| {
                                item.get("key").and_then(JsonValue::as_str) == Some("rapier")
                            })
                            .count(),
                    )
                    .unwrap_or(i64::MAX)
                });
            transaction.execute(
                "INSERT INTO wrapped_enrichment_facts(
                   guild_id,match_id,discord_id,actions_per_min,courier_kills,
                   pings,rapier_count,lane_role,comeback,throw
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(guild_id,match_id,discord_id) DO UPDATE SET
                   actions_per_min=excluded.actions_per_min,
                   courier_kills=excluded.courier_kills,
                   pings=excluded.pings,rapier_count=excluded.rapier_count,
                   lane_role=excluded.lane_role,comeback=excluded.comeback,
                   throw=excluded.throw",
                params![
                    guild_id,
                    match_id,
                    discord_id,
                    json_number_f64(player.and_then(|fields| fields.get("actions_per_min"))),
                    json_number_i64(player.and_then(|fields| fields.get("courier_kills"))),
                    json_number_i64(player.and_then(|fields| fields.get("pings"))),
                    rapier_count,
                    json_number_i64(player.and_then(|fields| fields.get("lane_role"))),
                    json_number_f64(payload.get("comeback")),
                    json_number_f64(payload.get("throw")),
                ],
            )?;
        }
    }
    Ok(())
}

const OPENSKILL_ALGORITHM_VERSION: i64 = 5;
const RECENT_OUTCOME_LIMIT: usize = 20;

#[derive(Clone, Debug)]
struct ReplayMatch {
    match_id: i64,
    guild_id: i64,
    winning_team: i64,
    match_date: Option<String>,
    match_at: Option<i64>,
    team1_players: String,
    team2_players: String,
    streak_rate: Option<f64>,
    streak_threshold: Option<i64>,
}

#[derive(Clone, Debug)]
struct ReplayParticipant {
    discord_id: i64,
    team_number: Option<i64>,
    side: Option<String>,
    fantasy_points: Option<f64>,
    gain_multiplier: f64,
}

impl ReplayParticipant {
    fn inferred_team(&self) -> Option<i64> {
        match self.team_number {
            Some(team @ (1 | 2)) => Some(team),
            _ if self.side.as_deref() == Some("radiant") => Some(1),
            _ if self.side.as_deref() == Some("dire") => Some(2),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct ReplayEvent {
    event_id: i64,
    guild_id: i64,
    discord_id: i64,
    event_type: String,
    value: f64,
    event_at: Option<i64>,
}

fn parse_datetime_timestamp(value: Option<&str>) -> Option<i64> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(&raw.replace(' ', "T")) {
        return Some(parsed.timestamp());
    }
    ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(raw, format).ok())
        .map(|parsed| DateTime::<Utc>::from_naive_utc_and_offset(parsed, Utc).timestamp())
}

fn parse_team_ids(raw: &str) -> Vec<i64> {
    serde_json::from_str::<JsonValue>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(json_coerce_i64)
        .collect()
}

fn load_replay_matches(transaction: &Transaction<'_>) -> Result<Vec<ReplayMatch>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT m.match_id,COALESCE(m.guild_id,0),m.winning_team,m.match_date,
                m.team1_players,m.team2_players,
                (SELECT rh.streak_multiplier_per_game FROM rating_history rh
                 WHERE rh.guild_id=m.guild_id AND rh.match_id=m.match_id
                 ORDER BY rh.id LIMIT 1),
                (SELECT rh.streak_threshold FROM rating_history rh
                 WHERE rh.guild_id=m.guild_id AND rh.match_id=m.match_id
                 ORDER BY rh.id LIMIT 1)
         FROM matches m WHERE m.winning_team IN (1,2)",
    )?;
    let mut matches = statement
        .query_map([], |row| {
            let match_date = row.get::<_, Option<String>>(3)?;
            Ok(ReplayMatch {
                match_id: row.get(0)?,
                guild_id: row.get(1)?,
                winning_team: row.get(2)?,
                match_at: parse_datetime_timestamp(match_date.as_deref()),
                match_date,
                team1_players: row.get(4)?,
                team2_players: row.get(5)?,
                streak_rate: row.get(6)?,
                streak_threshold: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    matches.sort_by_key(|game| {
        (
            game.guild_id,
            game.match_at.is_none(),
            game.match_at.unwrap_or(i64::MAX),
            game.match_id,
        )
    });
    Ok(matches)
}

fn load_replay_events(
    transaction: &Transaction<'_>,
) -> Result<BTreeMap<i64, Vec<ReplayEvent>>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT event_id,COALESCE(guild_id,0),discord_id,event_type,value,event_at
         FROM openskill_rating_events",
    )?;
    let mut events = statement
        .query_map([], |row| {
            let event_at = row.get::<_, Option<String>>(5)?;
            Ok(ReplayEvent {
                event_id: row.get(0)?,
                guild_id: row.get(1)?,
                discord_id: row.get(2)?,
                event_type: row.get(3)?,
                value: row.get(4)?,
                event_at: parse_datetime_timestamp(event_at.as_deref()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    events.sort_by_key(|event| {
        (
            event.guild_id,
            event.event_at.is_none(),
            event.event_at.unwrap_or(i64::MAX),
            event.event_id,
        )
    });
    let mut by_guild: BTreeMap<i64, Vec<ReplayEvent>> = BTreeMap::new();
    for event in events {
        by_guild.entry(event.guild_id).or_default().push(event);
    }
    Ok(by_guild)
}

fn load_replay_participants(
    transaction: &Transaction<'_>,
    match_id: i64,
    guild_id: i64,
) -> Result<Vec<ReplayParticipant>, rusqlite::Error> {
    let mut statement = transaction.prepare(
        "SELECT mp.discord_id,mp.team_number,mp.side,mp.fantasy_points,
                COALESCE((
                  SELECT rh.low_priority_gain_multiplier
                  FROM rating_history rh
                  WHERE rh.guild_id=mp.guild_id AND rh.match_id=mp.match_id
                    AND rh.discord_id=mp.discord_id
                  ORDER BY rh.id LIMIT 1
                ),1.0)
         FROM match_participants mp
         WHERE mp.match_id=?1 AND mp.guild_id=?2
         ORDER BY mp.discord_id",
    )?;
    statement
        .query_map(params![match_id, guild_id], |row| {
            Ok(ReplayParticipant {
                discord_id: row.get(0)?,
                team_number: row.get(1)?,
                side: row.get(2)?,
                fantasy_points: row.get(3)?,
                gain_multiplier: row.get::<_, Option<f64>>(4)?.unwrap_or(1.0),
            })
        })?
        .collect()
}

fn apply_replay_event(
    event: &ReplayEvent,
    current: &mut BTreeMap<(i64, i64), OpenSkillRating>,
) -> Result<(), SchemaMigrationError> {
    let key = (event.guild_id, event.discord_id);
    let Some(rating) = current.get_mut(&key) else {
        return Err(SchemaMigrationError::OpenSkillReplay(format!(
            "event {} references unknown player {}",
            event.event_id, event.discord_id
        )));
    };
    if !event.value.is_finite() {
        return Err(SchemaMigrationError::OpenSkillReplay(format!(
            "event {} has a non-finite value",
            event.event_id
        )));
    }
    match event.event_type.as_str() {
        "set_mu" => rating.mu = event.value,
        "set_sigma" => {
            rating.sigma = event.value.clamp(0.0, CamaOpenSkillSystem::DEFAULT_SIGMA);
        }
        "add_sigma" => {
            rating.sigma =
                (rating.sigma + event.value).clamp(0.0, CamaOpenSkillSystem::DEFAULT_SIGMA);
        }
        unknown => {
            return Err(SchemaMigrationError::OpenSkillReplay(format!(
                "event {} has unknown type {unknown:?}",
                event.event_id
            )));
        }
    }
    Ok(())
}

fn replay_openskill(
    transaction: &Transaction<'_>,
    settings: &MigrationSettings,
) -> Result<(), SchemaMigrationError> {
    let system = CamaOpenSkillSystem::with_config(settings.openskill.clone())
        .map_err(|error| SchemaMigrationError::OpenSkillReplay(error.to_string()))?;
    let glicko = CamaRatingSystem::default();
    let algorithm_fingerprint = system.algorithm_fingerprint();
    let mut current = BTreeMap::new();
    {
        let mut statement = transaction.prepare(
            "SELECT discord_id,COALESCE(guild_id,0),initial_mmr FROM players
             ORDER BY guild_id,discord_id",
        )?;
        let players = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (discord_id, guild_id, initial_mmr) in players {
            let source_mmr = initial_mmr.unwrap_or(4_000).clamp(0, 12_000);
            let seed_mmr = (source_mmr - i64::from(settings.new_player_mmr_discount)).max(0);
            let seed_mmr = i32::try_from(seed_mmr).unwrap_or(i32::MAX);
            current.insert(
                (guild_id, discord_id),
                OpenSkillRating::new(
                    system.mmr_to_os_mu(seed_mmr),
                    CamaOpenSkillSystem::DEFAULT_SIGMA,
                ),
            );
        }
    }

    let matches = load_replay_matches(transaction)?;
    let events = load_replay_events(transaction)?;
    let mut event_offsets: BTreeMap<i64, usize> = events
        .keys()
        .copied()
        .map(|guild_id| (guild_id, 0_usize))
        .collect();
    let mut last_match_at: BTreeMap<(i64, i64), i64> = BTreeMap::new();
    let mut recent_outcomes: BTreeMap<(i64, i64), Vec<bool>> = BTreeMap::new();

    for game in matches {
        if let Some(guild_events) = events.get(&game.guild_id) {
            let offset = event_offsets.entry(game.guild_id).or_default();
            while *offset < guild_events.len() {
                let event = &guild_events[*offset];
                if game.match_at.is_some_and(|match_at| {
                    event.event_at.is_none_or(|event_at| event_at > match_at)
                }) {
                    break;
                }
                apply_replay_event(event, &mut current)?;
                *offset += 1;
            }
        }

        let participants = load_replay_participants(transaction, game.match_id, game.guild_id)?;
        let participant_by_id: BTreeMap<i64, &ReplayParticipant> = participants
            .iter()
            .map(|participant| (participant.discord_id, participant))
            .collect();
        let participant_team1: Vec<i64> = participants
            .iter()
            .filter(|participant| participant.inferred_team() == Some(1))
            .map(|participant| participant.discord_id)
            .collect();
        let participant_team2: Vec<i64> = participants
            .iter()
            .filter(|participant| participant.inferred_team() == Some(2))
            .map(|participant| participant.discord_id)
            .collect();
        let (team1_ids, team2_ids) = if participant_team1.len() == 5 && participant_team2.len() == 5
        {
            (participant_team1, participant_team2)
        } else {
            (
                parse_team_ids(&game.team1_players),
                parse_team_ids(&game.team2_players),
            )
        };
        let unique_ids: BTreeSet<i64> = team1_ids.iter().chain(&team2_ids).copied().collect();
        if team1_ids.len() != 5 || team2_ids.len() != 5 || unique_ids.len() != 10 {
            return Err(SchemaMigrationError::OpenSkillReplay(format!(
                "match {} has invalid team sizes {}/{}",
                game.match_id,
                team1_ids.len(),
                team2_ids.len()
            )));
        }

        let mut before = BTreeMap::new();
        for player_id in team1_ids.iter().chain(&team2_ids).copied() {
            let key = (game.guild_id, player_id);
            let mut rating = current.get(&key).copied().unwrap_or(OpenSkillRating::new(
                CamaOpenSkillSystem::DEFAULT_MU,
                CamaOpenSkillSystem::DEFAULT_SIGMA,
            ));
            if let (Some(match_at), Some(previous_at)) = (game.match_at, last_match_at.get(&key)) {
                let days_since = (match_at - previous_at).div_euclid(86_400).max(0);
                rating.sigma = system.apply_sigma_decay(rating.sigma, days_since);
            }
            before.insert(player_id, rating);
        }

        let winning_team = WinningTeam::try_from(u8::try_from(game.winning_team).unwrap_or(0))
            .map_err(|error| SchemaMigrationError::OpenSkillReplay(error.to_string()))?;
        let mut won_by_player = BTreeMap::new();
        for (team, player_ids) in [(1_i64, &team1_ids), (2_i64, &team2_ids)] {
            for player_id in player_ids {
                won_by_player.insert(*player_id, team == game.winning_team);
            }
        }
        let streak_rate = game
            .streak_rate
            .unwrap_or(LEGACY_STREAK_MULTIPLIER_PER_GAME);
        let streak_threshold = game.streak_threshold.unwrap_or(LEGACY_STREAK_THRESHOLD);
        let mut streak_multipliers = BTreeMap::new();
        let mut gain_multipliers = BTreeMap::new();
        for player_id in team1_ids.iter().chain(&team2_ids).copied() {
            let won = won_by_player[&player_id];
            let adjustment = glicko.calculate_streak_multiplier(
                recent_outcomes
                    .get(&(game.guild_id, player_id))
                    .map_or(&[], Vec::as_slice),
                won,
                Some(streak_rate),
                Some(streak_threshold),
            );
            let domain_id = player_id as u64;
            streak_multipliers.insert(domain_id, adjustment.multiplier);
            gain_multipliers.insert(
                domain_id,
                participant_by_id
                    .get(&player_id)
                    .map_or(1.0, |participant| participant.gain_multiplier),
            );
        }

        let before_team1 = team1_ids.iter().map(|id| before[id]).collect::<Vec<_>>();
        let before_team2 = team2_ids.iter().map(|id| before[id]).collect::<Vec<_>>();
        let raw_probability = system.os_predict_win_probability(&before_team1, &before_team2);
        let calibrated_probability = system
            .calibrate_win_probability(raw_probability, None)
            .map_err(|error| SchemaMigrationError::OpenSkillReplay(error.to_string()))?;
        transaction.execute(
            "INSERT INTO match_predictions(
               match_id,openskill_radiant_win_prob,openskill_raw_radiant_win_prob,
               openskill_algorithm_version,openskill_algorithm_fingerprint
             ) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(match_id) DO UPDATE SET
               openskill_radiant_win_prob=excluded.openskill_radiant_win_prob,
               openskill_raw_radiant_win_prob=excluded.openskill_raw_radiant_win_prob,
               openskill_algorithm_version=excluded.openskill_algorithm_version,
               openskill_algorithm_fingerprint=excluded.openskill_algorithm_fingerprint",
            params![
                game.match_id,
                calibrated_probability,
                raw_probability,
                OPENSKILL_ALGORITHM_VERSION,
                algorithm_fingerprint
            ],
        )?;

        let complete_fantasy = participant_by_id.len() >= 10
            && team1_ids.iter().chain(&team2_ids).all(|player_id| {
                participant_by_id
                    .get(player_id)
                    .and_then(|participant| participant.fantasy_points)
                    .is_some()
            });
        let mut updated = BTreeMap::new();
        let mut fantasy_weights: BTreeMap<i64, Option<f64>> = BTreeMap::new();
        if complete_fantasy {
            let team1 = team1_ids
                .iter()
                .map(|player_id| {
                    WeightedPlayer::new(
                        *player_id as u64,
                        Some(before[player_id].mu),
                        Some(before[player_id].sigma),
                        participant_by_id[player_id].fantasy_points,
                    )
                })
                .collect::<Vec<_>>();
            let team2 = team2_ids
                .iter()
                .map(|player_id| {
                    WeightedPlayer::new(
                        *player_id as u64,
                        Some(before[player_id].mu),
                        Some(before[player_id].sigma),
                        participant_by_id[player_id].fantasy_points,
                    )
                })
                .collect::<Vec<_>>();
            let results = system
                .update_ratings_after_match(
                    &team1,
                    &team2,
                    winning_team,
                    &streak_multipliers,
                    &gain_multipliers,
                )
                .map_err(|error| SchemaMigrationError::OpenSkillReplay(error.to_string()))?;
            for player_id in team1_ids.iter().chain(&team2_ids).copied() {
                let result = results.get(&(player_id as u64)).ok_or_else(|| {
                    SchemaMigrationError::OpenSkillReplay(format!(
                        "match {} omitted player {player_id}",
                        game.match_id
                    ))
                })?;
                updated.insert(player_id, result.rating);
                fantasy_weights.insert(player_id, result.contribution_weight);
            }
        } else {
            let team1 = team1_ids
                .iter()
                .map(|player_id| {
                    OpenSkillPlayer::new(
                        *player_id as u64,
                        Some(before[player_id].mu),
                        Some(before[player_id].sigma),
                    )
                })
                .collect::<Vec<_>>();
            let team2 = team2_ids
                .iter()
                .map(|player_id| {
                    OpenSkillPlayer::new(
                        *player_id as u64,
                        Some(before[player_id].mu),
                        Some(before[player_id].sigma),
                    )
                })
                .collect::<Vec<_>>();
            let results = system
                .update_ratings_equal_weight(
                    &team1,
                    &team2,
                    winning_team,
                    &streak_multipliers,
                    &gain_multipliers,
                )
                .map_err(|error| SchemaMigrationError::OpenSkillReplay(error.to_string()))?;
            for player_id in team1_ids.iter().chain(&team2_ids).copied() {
                updated.insert(player_id, results[&(player_id as u64)]);
                fantasy_weights.insert(player_id, None);
            }
        }

        for (team_number, player_ids) in [(1_i64, &team1_ids), (2_i64, &team2_ids)] {
            for player_id in player_ids {
                let key = (game.guild_id, *player_id);
                let old = before[player_id];
                let new = updated[player_id];
                current.insert(key, new);
                if let Some(match_at) = game.match_at {
                    last_match_at.insert(key, match_at);
                }
                let won = i64::from(team_number == game.winning_team);
                let changed = transaction.execute(
                    "UPDATE rating_history SET
                       os_mu_before=?1,os_mu_after=?2,
                       os_sigma_before=?3,os_sigma_after=?4,
                       fantasy_weight=?5,os_algorithm_version=?6,
                       os_algorithm_fingerprint=?7,
                       team_number=COALESCE(team_number,?8),won=COALESCE(won,?9)
                     WHERE guild_id=?10 AND match_id=?11 AND discord_id=?12",
                    params![
                        old.mu,
                        new.mu,
                        old.sigma,
                        new.sigma,
                        fantasy_weights[player_id],
                        OPENSKILL_ALGORITHM_VERSION,
                        algorithm_fingerprint,
                        team_number,
                        won,
                        game.guild_id,
                        game.match_id,
                        player_id
                    ],
                )?;
                if changed == 0 {
                    transaction.execute(
                        "INSERT INTO rating_history(
                           discord_id,guild_id,match_id,timestamp,team_number,won,
                           os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                           fantasy_weight,os_algorithm_version,os_algorithm_fingerprint
                         ) VALUES (?1,?2,?3,COALESCE(?4,CURRENT_TIMESTAMP),?5,?6,
                                   ?7,?8,?9,?10,?11,?12,?13)",
                        params![
                            player_id,
                            game.guild_id,
                            game.match_id,
                            game.match_date,
                            team_number,
                            won,
                            old.mu,
                            new.mu,
                            old.sigma,
                            new.sigma,
                            fantasy_weights[player_id],
                            OPENSKILL_ALGORITHM_VERSION,
                            algorithm_fingerprint
                        ],
                    )?;
                }
                let outcomes = recent_outcomes.entry(key).or_default();
                outcomes.insert(0, won != 0);
                outcomes.truncate(RECENT_OUTCOME_LIMIT);
            }
        }
    }

    for (guild_id, guild_events) in &events {
        let offset = event_offsets.get(guild_id).copied().unwrap_or_default();
        for event in guild_events.iter().skip(offset) {
            apply_replay_event(event, &mut current)?;
        }
    }
    for ((guild_id, discord_id), rating) in &current {
        transaction.execute(
            "UPDATE players SET os_mu=?1,os_sigma=?2,os_rating_version=?3,
                    os_algorithm_fingerprint=?4,updated_at=CURRENT_TIMESTAMP
             WHERE guild_id=?5 AND discord_id=?6",
            params![
                rating.mu,
                rating.sigma,
                OPENSKILL_ALGORITHM_VERSION,
                algorithm_fingerprint,
                guild_id,
                discord_id
            ],
        )?;
    }
    transaction.execute_batch(
        "INSERT INTO openskill_rating_revisions(guild_id,revision)
         SELECT guild_id,1 FROM players GROUP BY guild_id
         ON CONFLICT(guild_id) DO UPDATE SET
           revision=openskill_rating_revisions.revision+1,
           updated_at=CURRENT_TIMESTAMP;",
    )?;
    Ok(())
}

trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "schema_manager/tests.rs"]
mod tests;
