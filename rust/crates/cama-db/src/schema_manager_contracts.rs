//! Read-only audits for the cross-language top-level schema contract.
//!
//! The centralized Rust [`crate::schema_manager`] owns production initialization
//! and migration. This module independently inspects the resulting file and
//! consumes durable rows so startup authority and compatibility evidence stay
//! separate. It deliberately exposes no DDL or migration writer itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cama_domain::rating::CamaRatingSystem;
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

const REQUIRED_TABLES: [&str; 15] = [
    "bets",
    "draft_finalization_jobs",
    "economy_ledger_context",
    "economy_ledger_entries",
    "lobby_state",
    "lobby_target_subscriptions",
    "match_participants",
    "match_predictions",
    "matches",
    "pending_matches",
    "players",
    "rating_history",
    "reminder_preferences",
    "schema_migrations",
    "wrapped_enrichment_facts",
];

const RETIRED_TABLES: [&str; 4] = [
    "curses",
    "protected_hero_purchases",
    "war_bets",
    "wheel_wars",
];

const REQUIRED_MIGRATIONS: [&str; 15] = [
    "add_base_rating_delta_multiplier_to_rating_history",
    "add_lobby_enabled_to_reminder_preferences",
    "add_low_priority_gain_multiplier_to_rating_history",
    "add_region_columns_to_players",
    "add_streak_multiplier_per_game_to_rating_history",
    "backfill_economy_ledger_opening_balances",
    "cap_package_deal_games_remaining",
    "cap_soft_avoid_games_remaining",
    "create_economy_ledger_tables",
    "create_draft_finalization_jobs",
    "create_lobby_target_subscriptions",
    "create_wrapped_enrichment_facts",
    "expand_economy_event_severity_levels",
    "normalize_null_guild_id_pairings_and_neon",
    "recompute_symmetric_glicko_predictions",
];

const REQUIRED_COLUMNS: [(&str, &str); 20] = [
    ("players", "preferred_region"),
    ("players", "inferred_region"),
    ("rating_history", "streak_multiplier_per_game"),
    ("rating_history", "base_rating_delta_multiplier"),
    ("rating_history", "low_priority_gain_multiplier"),
    ("reminder_preferences", "lobby_enabled"),
    ("wrapped_enrichment_facts", "guild_id"),
    ("wrapped_enrichment_facts", "match_id"),
    ("wrapped_enrichment_facts", "discord_id"),
    ("wrapped_enrichment_facts", "actions_per_min"),
    ("wrapped_enrichment_facts", "courier_kills"),
    ("wrapped_enrichment_facts", "pings"),
    ("wrapped_enrichment_facts", "rapier_count"),
    ("wrapped_enrichment_facts", "lane_role"),
    ("wrapped_enrichment_facts", "comeback"),
    ("wrapped_enrichment_facts", "throw"),
    ("match_predictions", "radiant_rating"),
    ("match_predictions", "dire_rating"),
    ("match_predictions", "radiant_rd"),
    ("match_predictions", "dire_rd"),
];

const REQUIRED_INDEXES: [&str; 2] = [
    "idx_draft_finalization_jobs_incomplete",
    "idx_reminder_prefs_lobby",
];

const REQUIRED_TRIGGERS: [&str; 6] = [
    "trg_economy_ledger_nonprofit_insert",
    "trg_economy_ledger_nonprofit_update",
    "trg_economy_ledger_players_insert",
    "trg_economy_ledger_players_update",
    "trg_package_deals_games_remaining_insert_cap",
    "trg_package_deals_games_remaining_update_cap",
];

/// Columns accepted by Python's `DigRepository.update_tunnel`.
///
/// This duplicate is intentional: a schema migration that adds a tunnel
/// column must remain writable from both runtimes or be explicitly excluded.
pub const TUNNEL_UPDATABLE_COLUMNS: &[&str] = &[
    "auto_buy_hard_hat",
    "auto_buy_torch",
    "best_run_score",
    "boss_attempts",
    "boss_progress",
    "cavein_free_streak",
    "cheer_data",
    "current_run_artifacts",
    "current_run_events",
    "current_run_jc",
    "depth",
    "grappling_hook_charges",
    "hard_hat_charges",
    "injury_state",
    "insured_until",
    "lantern_stub_date",
    "last_cheer_at",
    "last_dig_at",
    "last_lum_update_at",
    "luminosity",
    "max_depth",
    "miner_about",
    "miner_origin",
    "mutations",
    "paid_dig_date",
    "paid_digs_today",
    "pickaxe_tier",
    "pinnacle_boss_id",
    "pinnacle_hp_remaining",
    "pinnacle_last_engaged_at",
    "pinnacle_phase",
    "prestige_level",
    "prestige_perks",
    "relic_trim_notice",
    "reinforced_until",
    "revenge_target",
    "revenge_type",
    "revenge_until",
    "route_state",
    "sonar_skip_pending",
    "stat_boss_awards",
    "stat_points",
    "stat_smarts",
    "stat_stamina",
    "stat_strength",
    "stinger_curse",
    "streak_days",
    "streak_last_date",
    "temp_buffs",
    "temp_curses",
    "thick_skin_date",
    "total_digs",
    "total_jc_earned",
    "total_prestige_score",
    "trap_active",
    "trap_date",
    "trap_free_today",
    "tunnel_name",
    "void_bait_digs",
];

/// Tunnel fields normalized to integers by Python's repository.
pub const TUNNEL_INTEGER_COLUMNS: &[&str] = &[
    "auto_buy_hard_hat",
    "auto_buy_torch",
    "best_run_score",
    "boss_attempts",
    "cavein_free_streak",
    "current_run_artifacts",
    "current_run_events",
    "current_run_jc",
    "depth",
    "discord_id",
    "grappling_hook_charges",
    "guild_id",
    "hard_hat_charges",
    "insured_until",
    "last_cheer_at",
    "last_dig_at",
    "last_lum_update_at",
    "luminosity",
    "max_depth",
    "paid_digs_today",
    "pickaxe_tier",
    "pinnacle_hp_remaining",
    "pinnacle_last_engaged_at",
    "pinnacle_phase",
    "prestige_level",
    "reinforced_until",
    "relic_trim_notice",
    "revenge_target",
    "revenge_until",
    "sonar_skip_pending",
    "stat_points",
    "stat_smarts",
    "stat_stamina",
    "stat_strength",
    "streak_days",
    "total_digs",
    "total_jc_earned",
    "total_prestige_score",
    "trap_active",
    "trap_free_today",
    "void_bait_digs",
];

const TUNNEL_EXCLUDED_COLUMNS: [&str; 5] = [
    "created_at",
    "discord_id",
    "engine_mode",
    "guild_id",
    "retreat_cooldown_until",
];

#[derive(Debug, Error)]
pub enum SchemaManagerContractError {
    #[error("database path does not exist or is not a file: {0}")]
    MissingDatabase(PathBuf),
    #[error("SQLite schema-manager contract audit failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemaManagerAudit {
    pub applied_migration_count: usize,
    pub missing_tables: Vec<String>,
    pub retired_tables: Vec<String>,
    pub missing_migrations: Vec<String>,
    pub duplicate_migrations: Vec<String>,
    pub missing_columns: Vec<String>,
    pub malformed_columns: Vec<String>,
    pub missing_indexes: Vec<String>,
    pub missing_triggers: Vec<String>,
    pub malformed_schema_objects: Vec<String>,
    pub tunnel_columns_without_writer: Vec<String>,
    pub tunnel_writers_without_column: Vec<String>,
    pub tunnel_integer_contract_without_column: Vec<String>,
}

impl SchemaManagerAudit {
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.applied_migration_count > 0
            && self.missing_tables.is_empty()
            && self.retired_tables.is_empty()
            && self.missing_migrations.is_empty()
            && self.duplicate_migrations.is_empty()
            && self.missing_columns.is_empty()
            && self.malformed_columns.is_empty()
            && self.missing_indexes.is_empty()
            && self.missing_triggers.is_empty()
            && self.malformed_schema_objects.is_empty()
            && self.tunnel_columns_without_writer.is_empty()
            && self.tunnel_writers_without_column.is_empty()
            && self.tunnel_integer_contract_without_column.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationLedgerSnapshot {
    pub applied_count: usize,
    pub distinct_count: usize,
    pub names: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EconomyLedgerRow {
    pub account_type: String,
    pub account_id: i64,
    pub delta: i64,
    pub balance_before: i64,
    pub balance_after: i64,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedEnrichmentFact {
    pub discord_id: i64,
    pub actions_per_min: Option<f64>,
    pub courier_kills: Option<i64>,
    pub pings: Option<i64>,
    pub rapier_count: i64,
    pub lane_role: Option<i64>,
    pub comeback: Option<f64>,
    pub throw: Option<f64>,
}

/// Audit a database already initialized by Python without taking write access.
pub fn audit_existing_schema(
    path: impl AsRef<Path>,
) -> Result<SchemaManagerAudit, SchemaManagerContractError> {
    let connection = open_read_only(path.as_ref())?;
    audit_connection(&connection).map_err(Into::into)
}

/// Read the Python migration ledger without changing it.
pub fn migration_ledger_snapshot(
    path: impl AsRef<Path>,
) -> Result<MigrationLedgerSnapshot, SchemaManagerContractError> {
    let connection = open_read_only(path.as_ref())?;
    let names = query_string_set(&connection, "SELECT name FROM schema_migrations")?;
    let applied_count =
        connection.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get::<_, i64>(0)
        })?;
    Ok(MigrationLedgerSnapshot {
        applied_count: usize::try_from(applied_count).unwrap_or_default(),
        distinct_count: names.len(),
        names,
    })
}

/// Read economy-ledger rows in durable insertion order.
pub fn load_economy_ledger(
    path: impl AsRef<Path>,
) -> Result<Vec<EconomyLedgerRow>, SchemaManagerContractError> {
    let connection = open_read_only(path.as_ref())?;
    connection
        .prepare(
            "SELECT account_type, account_id, delta, balance_before, balance_after, source
             FROM economy_ledger_entries ORDER BY ledger_id",
        )?
        .query_map([], |row| {
            Ok(EconomyLedgerRow {
                account_type: row.get(0)?,
                account_id: row.get(1)?,
                delta: row.get(2)?,
                balance_before: row.get(3)?,
                balance_after: row.get(4)?,
                source: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Read the compact Wrapped projection produced by Python's migration.
pub fn load_wrapped_enrichment_facts(
    path: impl AsRef<Path>,
    guild_id: i64,
    match_id: i64,
) -> Result<Vec<WrappedEnrichmentFact>, SchemaManagerContractError> {
    let connection = open_read_only(path.as_ref())?;
    connection
        .prepare(
            "SELECT discord_id, actions_per_min, courier_kills, pings,
                    rapier_count, lane_role, comeback, throw
             FROM wrapped_enrichment_facts
             WHERE guild_id=?1 AND match_id=?2 ORDER BY discord_id",
        )?
        .query_map([guild_id, match_id], |row| {
            Ok(WrappedEnrichmentFact {
                discord_id: row.get(0)?,
                actions_per_min: row.get(1)?,
                courier_kills: row.get(2)?,
                pings: row.get(3)?,
                rapier_count: row.get(4)?,
                lane_role: row.get(5)?,
                comeback: row.get(6)?,
                throw: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Reproduce the side-symmetric historical prediction values consumed by Rust.
#[must_use]
pub fn symmetric_prediction_probabilities(
    radiant_rating: f64,
    radiant_rd: f64,
    dire_rating: f64,
    dire_rd: f64,
) -> (f64, f64) {
    let radiant =
        CamaRatingSystem::predict_win_probability(radiant_rating, radiant_rd, dire_rating, dire_rd);
    (radiant, 1.0 - radiant)
}

/// Opening balance required when an account already has partial ledger history.
#[must_use]
pub fn ledger_opening_balance(current_balance: i64, recorded_deltas: i64) -> i64 {
    current_balance - recorded_deltas
}

fn open_read_only(path: &Path) -> Result<Connection, SchemaManagerContractError> {
    if !path.is_file() {
        return Err(SchemaManagerContractError::MissingDatabase(
            path.to_path_buf(),
        ));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn audit_connection(connection: &Connection) -> rusqlite::Result<SchemaManagerAudit> {
    let tables = query_string_set(
        connection,
        "SELECT name FROM sqlite_master WHERE type='table'",
    )?;
    let indexes = query_string_set(
        connection,
        "SELECT name FROM sqlite_master WHERE type='index'",
    )?;
    let triggers = query_string_set(
        connection,
        "SELECT name FROM sqlite_master WHERE type='trigger'",
    )?;
    let migration_counts = query_name_counts(connection)?;

    let mut audit = SchemaManagerAudit {
        applied_migration_count: migration_counts.values().sum(),
        missing_tables: missing(&tables, &REQUIRED_TABLES),
        retired_tables: present(&tables, &RETIRED_TABLES),
        missing_migrations: missing_counts(&migration_counts, &REQUIRED_MIGRATIONS),
        duplicate_migrations: migration_counts
            .iter()
            .filter(|(_, count)| **count != 1)
            .map(|(name, _)| name.clone())
            .collect(),
        missing_indexes: missing(&indexes, &REQUIRED_INDEXES),
        missing_triggers: missing(&triggers, &REQUIRED_TRIGGERS),
        ..SchemaManagerAudit::default()
    };

    let mut columns_by_table = BTreeMap::new();
    for (table, column) in REQUIRED_COLUMNS {
        let columns = columns_by_table
            .entry(table)
            .or_insert(table_columns(connection, table)?);
        if !columns.contains_key(column) {
            audit.missing_columns.push(format!("{table}.{column}"));
        }
    }
    check_column_default(
        &mut audit,
        columns_by_table.get("rating_history"),
        "rating_history",
        "streak_multiplier_per_game",
        "0.20",
    );
    check_column_default(
        &mut audit,
        columns_by_table.get("rating_history"),
        "rating_history",
        "base_rating_delta_multiplier",
        "0.75",
    );
    check_column_default(
        &mut audit,
        columns_by_table.get("rating_history"),
        "rating_history",
        "low_priority_gain_multiplier",
        "1.0",
    );
    check_column_default(
        &mut audit,
        columns_by_table.get("reminder_preferences"),
        "reminder_preferences",
        "lobby_enabled",
        "0",
    );

    check_lobby_target_shape(connection, &tables, &mut audit)?;
    check_schema_object_sql(connection, &triggers, &mut audit)?;
    check_tunnel_contract(connection, &tables, &mut audit)?;
    Ok(audit)
}

#[derive(Clone, Debug)]
struct ColumnInfo {
    default: Option<String>,
    not_null: bool,
    pk_position: i64,
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> rusqlite::Result<BTreeMap<String, ColumnInfo>> {
    connection
        .prepare(&format!("PRAGMA table_info('{table}')"))?
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                ColumnInfo {
                    not_null: row.get::<_, i64>(3)? != 0,
                    default: row.get(4)?,
                    pk_position: row.get(5)?,
                },
            ))
        })?
        .collect()
}

fn query_string_set(connection: &Connection, sql: &str) -> rusqlite::Result<BTreeSet<String>> {
    connection
        .prepare(sql)?
        .query_map([], |row| row.get(0))?
        .collect()
}

fn query_name_counts(connection: &Connection) -> rusqlite::Result<BTreeMap<String, usize>> {
    connection
        .prepare("SELECT name, COUNT(*) FROM schema_migrations GROUP BY name")?
        .query_map([], |row| {
            let count = row.get::<_, i64>(1)?;
            Ok((row.get(0)?, usize::try_from(count).unwrap_or_default()))
        })?
        .collect()
}

fn object_sql(connection: &Connection, object_type: &str, name: &str) -> rusqlite::Result<String> {
    connection.query_row(
        "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type=?1 AND name=?2",
        [object_type, name],
        |row| row.get(0),
    )
}

fn missing<const N: usize>(actual: &BTreeSet<String>, required: &[&str; N]) -> Vec<String> {
    required
        .iter()
        .filter(|name| !actual.contains(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

fn present<const N: usize>(actual: &BTreeSet<String>, forbidden: &[&str; N]) -> Vec<String> {
    forbidden
        .iter()
        .filter(|name| actual.contains(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

fn missing_counts<const N: usize>(
    actual: &BTreeMap<String, usize>,
    required: &[&str; N],
) -> Vec<String> {
    required
        .iter()
        .filter(|name| !actual.contains_key(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

fn normalize_default(raw: &str) -> String {
    raw.trim()
        .trim_matches('(')
        .trim_matches(')')
        .trim_matches('\'')
        .trim_matches('"')
        .to_owned()
}

fn check_column_default(
    audit: &mut SchemaManagerAudit,
    columns: Option<&BTreeMap<String, ColumnInfo>>,
    table: &str,
    column: &str,
    expected: &str,
) {
    let Some(info) = columns.and_then(|values| values.get(column)) else {
        return;
    };
    let actual = info.default.as_deref().map(normalize_default);
    if actual.as_deref() != Some(expected) {
        audit
            .malformed_columns
            .push(format!("{table}.{column}:default={actual:?}"));
    }
}

fn check_lobby_target_shape(
    connection: &Connection,
    tables: &BTreeSet<String>,
    audit: &mut SchemaManagerAudit,
) -> rusqlite::Result<()> {
    if !tables.contains("lobby_target_subscriptions") {
        return Ok(());
    }
    let columns = table_columns(connection, "lobby_target_subscriptions")?;
    let expected = [
        ("guild_id", true, 1),
        ("target_id", true, 2),
        ("subscriber_id", true, 3),
        ("created_at", true, 0),
    ];
    for (name, not_null, pk_position) in expected {
        match columns.get(name) {
            Some(info) if info.not_null == not_null && info.pk_position == pk_position => {}
            Some(info) => audit.malformed_columns.push(format!(
                "lobby_target_subscriptions.{name}:not_null={},pk_position={}",
                info.not_null, info.pk_position
            )),
            None => audit
                .missing_columns
                .push(format!("lobby_target_subscriptions.{name}")),
        }
    }
    Ok(())
}

fn check_schema_object_sql(
    connection: &Connection,
    triggers: &BTreeSet<String>,
    audit: &mut SchemaManagerAudit,
) -> rusqlite::Result<()> {
    let economy_sql = object_sql(connection, "table", "economy_daily_events")?
        .to_ascii_lowercase()
        .replace(char::is_whitespace, " ");
    if !economy_sql.contains("severity") || !economy_sql.contains("between 1 and 5") {
        audit
            .malformed_schema_objects
            .push("economy_daily_events.severity".to_owned());
    }
    for trigger in [
        "trg_package_deals_games_remaining_insert_cap",
        "trg_package_deals_games_remaining_update_cap",
    ] {
        if !triggers.contains(trigger) {
            continue;
        }
        let sql = object_sql(connection, "trigger", trigger)?
            .to_ascii_lowercase()
            .replace(char::is_whitespace, " ");
        if !sql.contains("new.games_remaining > 10") {
            audit.malformed_schema_objects.push(trigger.to_owned());
        }
    }
    Ok(())
}

fn check_tunnel_contract(
    connection: &Connection,
    tables: &BTreeSet<String>,
    audit: &mut SchemaManagerAudit,
) -> rusqlite::Result<()> {
    if !tables.contains("tunnels") {
        audit.missing_tables.push("tunnels".to_owned());
        return Ok(());
    }
    let columns: BTreeSet<String> = table_columns(connection, "tunnels")?.into_keys().collect();
    let updatable: BTreeSet<String> = TUNNEL_UPDATABLE_COLUMNS
        .iter()
        .map(|column| (*column).to_owned())
        .collect();
    let excluded: BTreeSet<String> = TUNNEL_EXCLUDED_COLUMNS
        .iter()
        .map(|column| (*column).to_owned())
        .collect();
    let integers: BTreeSet<String> = TUNNEL_INTEGER_COLUMNS
        .iter()
        .map(|column| (*column).to_owned())
        .collect();
    audit.tunnel_columns_without_writer = columns
        .difference(&updatable)
        .filter(|column| !excluded.contains(*column))
        .cloned()
        .collect();
    audit.tunnel_writers_without_column = updatable.difference(&columns).cloned().collect();
    audit.tunnel_integer_contract_without_column = integers.difference(&columns).cloned().collect();
    Ok(())
}

#[cfg(test)]
#[path = "schema_manager_contracts/tests.rs"]
mod tests;
