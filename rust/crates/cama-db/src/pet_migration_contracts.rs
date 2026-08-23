//! Read-only contracts for pet schema migrations.
//!
//! The centralized Rust schema manager owns production migration. This module
//! independently audits the migrated SQLite file and models one-shot backfills
//! in pure data structures; production code here performs no DDL or
//! migration-ledger writes itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

pub const ADULT_AGE_SECONDS: i64 = 7 * 86_400;

const TABLES: [&str; 7] = [
    "pet_brawls",
    "pet_evolution_daily",
    "pet_refund_windows",
    "pet_supplies",
    "pet_stable_state",
    "pets",
    "reminder_preferences",
];

const INDEXES: [&str; 8] = [
    "idx_pet_brawls_open",
    "idx_pet_brawls_record_loss",
    "idx_pet_brawls_record_win",
    "idx_pets_evolution_due",
    "idx_pets_evolution_unannounced",
    "idx_pets_one_active",
    "idx_pets_unannounced_deaths",
    "idx_reminder_prefs_pet",
];

const PET_COLUMNS: [&str; 24] = [
    "dig_work_at",
    "dig_work_units",
    "egg_tier",
    "evolution_announced_at",
    "evolution_calling",
    "evolution_due_at",
    "evolution_primary",
    "evolution_secondary",
    "evolution_started_at",
    "evolved_at",
    "solo_training_recharged_at",
    "solo_training_sessions",
    "training_dex",
    "training_int",
    "training_str",
    "training_xp",
    "died_at",
    "death_announced_at",
    "hatched_at",
    "hunger_at_last_fed",
    "name",
    "species",
    "is_active",
    "stabled_at",
];

const BRAWL_COLUMNS: [&str; 24] = [
    "brawl_id",
    "challenger_id",
    "challenger_pet_id",
    "challenger_stat_gain",
    "challenger_xp_delta",
    "channel_id",
    "created_at",
    "expires_at",
    "fee",
    "guild_id",
    "loser_hunger_delta",
    "loser_pet_id",
    "personality_event_key",
    "recipient_id",
    "recipient_pet_id",
    "recipient_stat_gain",
    "recipient_xp_delta",
    "resolved_at",
    "rounds",
    "status",
    "wager",
    "winner_hunger_delta",
    "winner_id",
    "winner_pet_id",
];

const EVOLUTION_COLUMNS: [&str; 9] = [
    "first_activity",
    "first_source_key",
    "guild_id",
    "instinct",
    "pet_id",
    "score",
    "second_activity",
    "second_source_key",
    "upbringing_day",
];

#[derive(Debug, Error)]
pub enum PetMigrationContractError {
    #[error("database path does not exist or is not a file: {0}")]
    MissingDatabase(PathBuf),
    #[error("SQLite pet migration audit failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PetMigrationAudit {
    pub missing_tables: Vec<String>,
    pub missing_columns: Vec<String>,
    pub missing_indexes: Vec<String>,
    pub malformed_indexes: Vec<String>,
    pub evolution_due_query_uses_index: bool,
    pub evolution_announcement_query_uses_index: bool,
}

impl PetMigrationAudit {
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.missing_tables.is_empty()
            && self.missing_columns.is_empty()
            && self.missing_indexes.is_empty()
            && self.malformed_indexes.is_empty()
            && self.evolution_due_query_uses_index
            && self.evolution_announcement_query_uses_index
    }
}

pub fn audit_existing_schema(
    path: impl AsRef<Path>,
) -> Result<PetMigrationAudit, PetMigrationContractError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(PetMigrationContractError::MissingDatabase(
            path.to_path_buf(),
        ));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    audit_connection(&connection).map_err(Into::into)
}

fn audit_connection(connection: &Connection) -> Result<PetMigrationAudit, rusqlite::Error> {
    let tables = query_names(
        connection,
        "SELECT name FROM sqlite_master WHERE type='table'",
    )?;
    let indexes = query_names(
        connection,
        "SELECT name FROM sqlite_master WHERE type='index'",
    )?;
    let mut audit = PetMigrationAudit {
        missing_tables: TABLES
            .iter()
            .filter(|table| !tables.contains(**table))
            .map(|table| (*table).to_owned())
            .collect(),
        missing_indexes: INDEXES
            .iter()
            .filter(|index| !indexes.contains(**index))
            .map(|index| (*index).to_owned())
            .collect(),
        ..PetMigrationAudit::default()
    };
    for (table, required) in [
        ("pets", PET_COLUMNS.as_slice()),
        ("pet_brawls", BRAWL_COLUMNS.as_slice()),
        ("pet_evolution_daily", EVOLUTION_COLUMNS.as_slice()),
        (
            "pet_refund_windows",
            ["announcement_payload", "announced_at"].as_slice(),
        ),
        ("reminder_preferences", ["pet_enabled"].as_slice()),
    ] {
        if !tables.contains(table) {
            continue;
        }
        let columns = table_columns(connection, table)?;
        audit.missing_columns.extend(
            required
                .iter()
                .filter(|column| !columns.contains_key(**column))
                .map(|column| format!("{table}.{column}")),
        );
    }
    if indexes.contains("idx_pets_one_active")
        && !index_sql(connection, "idx_pets_one_active")?
            .to_ascii_lowercase()
            .contains("where died_at is null and is_active = 1")
    {
        audit
            .malformed_indexes
            .push("idx_pets_one_active".to_owned());
    }
    let due_plan = query_plan(
        connection,
        "EXPLAIN QUERY PLAN SELECT pet_id FROM pets WHERE died_at IS NULL AND is_active=1 AND evolved_at IS NULL AND evolution_due_at <= 2000000000",
    )?;
    audit.evolution_due_query_uses_index = due_plan
        .iter()
        .any(|detail| detail.contains("idx_pets_evolution_due"));
    let announcement_plan = query_plan(
        connection,
        "EXPLAIN QUERY PLAN SELECT pet_id FROM pets WHERE evolved_at IS NOT NULL AND evolution_announced_at IS NULL ORDER BY evolved_at, pet_id",
    )?;
    audit.evolution_announcement_query_uses_index = announcement_plan
        .iter()
        .any(|detail| detail.contains("idx_pets_evolution_unannounced"));
    Ok(audit)
}

fn query_names(connection: &Connection, sql: &str) -> Result<BTreeSet<String>, rusqlite::Error> {
    connection
        .prepare(sql)?
        .query_map([], |row| row.get(0))?
        .collect()
}

fn table_columns(
    connection: &Connection,
    table: &str,
) -> Result<BTreeMap<String, Option<String>>, rusqlite::Error> {
    connection
        .prepare(&format!("PRAGMA table_info('{table}')"))?
        .query_map([], |row| Ok((row.get(1)?, row.get(4)?)))?
        .collect()
}

fn index_sql(connection: &Connection, index: &str) -> Result<String, rusqlite::Error> {
    connection.query_row(
        "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type='index' AND name=?1",
        [index],
        |row| row.get(0),
    )
}

fn query_plan(connection: &Connection, sql: &str) -> Result<Vec<String>, rusqlite::Error> {
    connection
        .prepare(sql)?
        .query_map([], |row| row.get(3))?
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HatchRollRow {
    pub species: String,
    pub egg_tier: String,
    pub adopt_fee: i64,
    pub hatched_at: i64,
    pub died_at: Option<i64>,
}

pub fn model_hatch_roll_backfill(rows: &mut [HatchRollRow], now: i64) {
    for row in rows {
        row.egg_tier = if row.adopt_fee > 100 {
            "gilded"
        } else {
            "standard"
        }
        .to_owned();
        if row.died_at.is_none() && row.hatched_at > now {
            row.species = "unhatched".to_owned();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvolutionBackfill {
    pub hatched_at: i64,
    pub died_at: Option<i64>,
    pub evolution_started_at: Option<i64>,
    pub evolution_due_at: Option<i64>,
}

#[must_use]
pub fn model_evolution_backfill(mut row: EvolutionBackfill, now: i64) -> EvolutionBackfill {
    if row.died_at.is_some() {
        return row;
    }
    let started = row.evolution_started_at.unwrap_or(row.hatched_at.max(now));
    row.evolution_started_at = Some(started);
    row.evolution_due_at = row
        .evolution_due_at
        .or_else(|| started.checked_add(ADULT_AGE_SECONDS));
    row
}

#[must_use]
pub const fn model_dig_work_backfill(dig_work_at: i64, migration_time: i64) -> i64 {
    if dig_work_at == 0 {
        migration_time
    } else {
        dig_work_at
    }
}

#[cfg(test)]
#[path = "pet_migration_contracts/tests.rs"]
mod tests;
